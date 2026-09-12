//! Starting, feeding and stopping the background threads.
//!
//! The thread population is fixed for the life of the process: one input
//! reader, one search worker, one verification worker, one index actor per
//! configured share, one change watcher, plus rayon's pool for the matcher.
//! Nothing is spawned per
//! keystroke, so thread growth is impossible by construction rather than by
//! discipline.
//!
//! All of them report into one channel, which the main loop is the only
//! receiver of.

use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, bounded};

use super::event::{AppEvent, Cmd, CmdList};
use crate::clipboard;
use crate::config::{SHUTDOWN_JOIN_BUDGET, Settings};
use crate::history;
use crate::index::actor::{IndexActor, IndexContext};
use crate::index::enumerate::DirSource;
use crate::index::store::IndexStore;
use crate::index::{actor, fake_source::FakeDirSource};
use crate::open;
use crate::search::verify::Verifier;
use crate::search::worker::{self, Backend, SearchRequest, WorkerHandle};

/// Keystrokes must never be dropped, so this is generous and the input thread
/// blocks rather than discarding. Progress events, which are the only thing
/// that could ever fill it, are rate limited at the source.
const EVENT_CAPACITY: usize = 256;

/// Everything running in the background.
pub struct Actors {
    pub events: Sender<AppEvent>,
    pub backend: Arc<Backend>,
    pub verifier: Arc<Verifier>,
    search: WorkerHandle<SearchRequest>,
    verify: WorkerHandle<SearchRequest>,
    index: IndexActor,
    /// Absent when no tree mapping is configured.
    tree_index: Option<IndexActor>,
    /// Opens what Enter chose. One thread for the life of the process, like
    /// the rest: assembling a document reads every page off the share, which
    /// is far too much work to spawn a thread for per keypress.
    opener: open::worker::Opener,
    /// Absent when history is switched off, or when there is nowhere to put
    /// it. Recall still works within the session either way.
    history: Option<history::Writer>,
    /// Detached on purpose: a thread blocked in `event::read` cannot be woken.
    _input: JoinHandle<()>,
}

impl Actors {
    /// Starts every background thread.
    pub fn start(
        settings: Settings,
        source: Arc<dyn DirSource>,
        volume_serial: Option<u32>,
    ) -> std::io::Result<(Self, Receiver<AppEvent>)> {
        let (tx, rx) = bounded(EVENT_CAPACITY);
        let store = Arc::new(IndexStore::default());

        // Collect cache files that belong to no configured directory. Without
        // this the cache grows forever as roots are edited, and the previous
        // layout - whose filenames carried no directory identity at all -
        // would linger indefinitely.
        if settings.persist
            && let Some(cache_dir) = &settings.cache_dir
        {
            // Every *configured* mapping, not only the one indexed right now.
            // `gc_orphans` deletes anything that looks like one of ours, so a
            // narrower list meant that launching with a different effective
            // configuration - an env override, an alternate `--config` - wiped
            // the other one's cache and guaranteed it a cold start.
            //
            // The two *derived* roots are added explicitly rather than trusted
            // to fall out of the mapping list. They are what the index actors
            // actually write under, and a sweep that disagrees with the
            // writers by one entry does not fail loudly - it silently
            // guarantees a cold start, which for the tree is one to three
            // minutes of round trips on every launch.
            let live: Vec<_> = settings
                .routes
                .enabled()
                .map(|m| m.path.clone())
                .chain([settings.custpro_path.clone(), settings.tree_path.clone()])
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| crate::index::persist::MappingKey::of(&p))
                .collect();
            crate::index::persist::gc_orphans(cache_dir, &live);
        }

        let backend = Arc::new(Backend {
            settings: settings.clone(),
            store: Arc::clone(&store),
            source: Arc::clone(&source),
        });

        let verifier = Arc::new(Verifier::new(
            Arc::clone(&source),
            settings.custpro_path.clone(),
            settings.server_filter,
        ));

        let search = worker::spawn_search(Arc::clone(&backend), tx.clone())?;
        let verify = worker::spawn_verify(Arc::clone(&backend), Arc::clone(&verifier), tx.clone())?;
        let index = actor::spawn(
            IndexContext::new(
                settings.clone(),
                Arc::clone(&store),
                Arc::clone(&source),
                volume_serial,
            )
            .with_log(Arc::new(crate::index::log::IndexLog::from_option(
                settings.index_log.as_deref(),
            ))),
            tx.clone(),
        )?;
        // A second actor, only when a tree mapping is configured. One thread
        // each rather than one for both: a walk runs for minutes, and sharing
        // would mean the flat share's freshness queued behind it - or a
        // five-minute backoff on an unreachable tree delaying a healthy one.
        let tree_index = (!settings.tree_path.as_os_str().is_empty())
            .then(|| {
                actor::spawn(
                    IndexContext::new(
                        settings.clone(),
                        Arc::clone(&store),
                        Arc::clone(&source),
                        None,
                    )
                    .for_tree()
                    .with_log(Arc::new(crate::index::log::IndexLog::from_option(
                        settings.index_log.as_deref(),
                    )))
                    .with_live_updates(&settings),
                    tx.clone(),
                )
            })
            .transpose()?;
        // Merged documents cannot be deleted once a viewer has them open, so
        // last session's are collected at the start of this one - the same
        // arrangement the index cache uses just above. Swept *before* the
        // opener exists, so the sweep cannot race a merge it just wrote.
        if let Some(cache_dir) = &settings.cache_dir {
            open::pdf::gc(cache_dir, open::worker::CACHE_LIFETIME);
        }
        let opener = open::worker::spawn(Arc::clone(&backend), tx.clone())?;
        let input = spawn_input(tx.clone())?;

        // Best effort: failing to start the writer costs recall next session,
        // so it must not stop the program starting.
        let history = settings
            .history
            .then(|| settings.history_path.clone())
            .flatten()
            .and_then(|path| history::spawn_writer(path).ok());

        Ok((
            Self {
                events: tx,
                backend,
                verifier,
                search,
                verify,
                index,
                tree_index,
                opener,
                history,
                _input: input,
            },
            rx,
        ))
    }

    /// Executes the commands a state transition produced.
    ///
    /// The only place in the program that touches a channel on behalf of the
    /// UI, which keeps the state transition itself free of side effects.
    pub fn dispatch(&self, cmds: CmdList) {
        for cmd in cmds {
            match cmd {
                Cmd::Search { query, epoch } => {
                    // The worker adopts the UI's generation, so its
                    // cancellation and the UI's staleness check agree.
                    self.search
                        .submit_generation(epoch, SearchRequest { query, epoch });
                }
                Cmd::Verify { query, epoch } => {
                    self.verify
                        .submit_generation(epoch, SearchRequest { query, epoch });
                }
                Cmd::RefreshIndex { force } => {
                    self.index.refresh(force);
                    // F5 means "re-examine the share", and there are two.
                    if let Some(tree) = &self.tree_index {
                        tree.refresh(force);
                    }
                }
                Cmd::Open(request) => self.opener.request(request, &self.events),
                Cmd::SaveViewer(viewer) => crate::config::write::save_viewer_async(
                    self.backend
                        .settings
                        .routes
                        .source()
                        .path()
                        .map(Path::to_path_buf),
                    viewer,
                    self.events.clone(),
                ),
                Cmd::Copy(text) => clipboard::copy_async(text, self.events.clone()),
                Cmd::ReadClipboard => clipboard::read_async(self.events.clone()),
                Cmd::SaveHistory(entries) => {
                    if let Some(writer) = &self.history {
                        writer.store(entries);
                    }
                }
                Cmd::Quit => {}
            }
        }
    }

    /// Stops everything, with a bounded wait.
    ///
    /// Returns false when a thread had to be abandoned. That is expected when
    /// one is blocked in an SMB call: it cannot be woken, the process is about
    /// to exit, and index writes are temp-then-rename so nothing is left
    /// half-written. Making the user wait out a 45-second network timeout
    /// would be strictly worse.
    pub fn shutdown(&mut self) -> bool {
        let budget = SHUTDOWN_JOIN_BUDGET;
        let mut clean = true;
        clean &= self.search.shutdown(budget);
        clean &= self.verify.shutdown(budget);
        clean &= self.index.shutdown(budget);
        if let Some(tree) = &mut self.tree_index {
            clean &= tree.shutdown(budget);
        }
        clean &= self.opener.shutdown(budget);
        if let Some(writer) = &mut self.history {
            writer.shutdown();
        }
        clean
    }
}

/// Reads terminal events and forwards them.
///
/// Uses a blocking read rather than a poll loop, so an idle application costs
/// nothing and a keystroke is delivered immediately.
fn spawn_input(tx: Sender<AppEvent>) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("files-input".into())
        .spawn(move || {
            loop {
                match crossterm::event::read() {
                    Ok(crossterm::event::Event::Key(key)) => {
                        // Blocking send, never try_send: a dropped keystroke is
                        // far worse than a moment's backpressure, and the main
                        // loop always drains.
                        if tx.send(AppEvent::Key(key)).is_err() {
                            return;
                        }
                    }
                    Ok(crossterm::event::Event::Paste(text)) => {
                        if tx.send(AppEvent::Paste(text)).is_err() {
                            return;
                        }
                    }
                    Ok(crossterm::event::Event::Mouse(mouse)) => {
                        if tx.send(AppEvent::Mouse(mouse)).is_err() {
                            return;
                        }
                    }
                    Ok(crossterm::event::Event::Resize(cols, rows)) => {
                        // The size is carried rather than discarded: mouse
                        // events arrive in screen coordinates, so the state
                        // machine has to know where the widgets are.
                        if tx.send(AppEvent::Resize { cols, rows }).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        })
}

/// Builds the platform's directory source.
pub fn default_source(settings: &Settings) -> Arc<dyn DirSource> {
    #[cfg(windows)]
    {
        Arc::new(crate::index::win_enum::WinDirSource::new(
            settings.enum_strategy,
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = settings;
        Arc::new(crate::index::std_enum::StdDirSource)
    }
}

/// An in-memory source, for exercising the whole stack without any drives.
pub fn fake_source_for_demo() -> Arc<dyn DirSource> {
    // Shaped like a real job so the interesting cases can be reached by hand
    // on a machine with no drives mapped:
    //
    //   * twenty pages, which is more than MAX_RESULTS - so the rows on screen
    //     stop at fifteen while the document opens whole;
    //   * a `.tif` page, which is not a member and must pass in silence;
    //   * files that merely start with the code, which belong to no document.
    let mut names: Vec<String> = vec!["11-D-0704.pdf".into()];
    names.extend((1..=20).map(|i| format!("11-D-0704_Page{i}.pdf")));
    names.push("11-D-0704_Page21.tif".into());
    names.push("11-D-0704 revision notes.pdf".into());
    names.push("11-D-0704 notes.txt".into());
    // Registered at the configured roots, and as a *tree* rather than a pair
    // of flat directories, so `--demo` exercises the recursive walk rather
    // than reporting an empty share. The nested codes are the ones no routing
    // rule would have guessed - which is the case worth being able to try by
    // hand.
    let nested: Vec<String> = [
        "archive\\2019\\odd name\\11-3-0704 survey.pdf",
        "archive\\2019\\odd name\\11-3-0704 notes.txt",
        "archive\\2020\\ab12-0704 spec.pdf",
        "ab12\\drawing.pdf",
        "ab12\\rev b\\drawing.pdf",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // The job folder goes in as tree paths too, so it is reachable both by the
    // routing rules (which resolve the code straight to `R:\11d`) and by a
    // recursive walk from the root. Registering it as a bare listing would
    // leave it with no entry in its parent - a directory a walk could never
    // find, which is a shape a real share cannot have.
    let mut paths: Vec<String> = nested;
    paths.extend(names.iter().map(|n| format!("11d\\{n}")));
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();

    Arc::new(
        FakeDirSource::new()
            .with_synthetic(crate::config::CUSTPRO_PATH, 2_000)
            .with_tree("R:\\", &paths),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::fake_source::FakeDirSource;
    use std::time::{Duration, Instant};

    fn settings() -> Settings {
        Settings {
            persist: false,
            ..Default::default()
        }
    }

    fn start() -> (Actors, Receiver<AppEvent>) {
        let source: Arc<dyn DirSource> = Arc::new(
            FakeDirSource::new()
                .with_dir(crate::config::CUSTPRO_PATH, &["alpha.pdf"])
                // A tree now, reachable from its root, because the job share
                // is walked rather than routed to. The matcher looks for the
                // typed code inside the filename, so the fixtures contain it.
                .with_tree(
                    "R:\\",
                    &["11d\\11-D-0704 one.pdf", "11d\\11-D-0704 two.pdf"],
                ),
        );
        Actors::start(settings(), source, Some(1)).unwrap()
    }

    #[test]
    fn a_dispatched_search_produces_a_result() {
        let (actors, rx) = start();

        // The job share is walked in the background now rather than fetched
        // on demand, so a search dispatched before the walk has published
        // anything legitimately finds nothing. Waiting for the index is the
        // honest fixture; asserting on the first answer would be asserting on
        // a race.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && actors.backend.store.tree().is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            actors.backend.store.tree().is_some(),
            "the tree should have been walked"
        );

        let mut cmds = CmdList::new();
        cmds.push(Cmd::Search {
            query: "11-D-0704".into(),
            epoch: 1,
        });
        actors.dispatch(cmds);

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut got = None;
        while Instant::now() < deadline {
            if let Ok(AppEvent::Search(msg)) = rx.recv_timeout(Duration::from_millis(50)) {
                got = Some(msg);
                break;
            }
        }
        let msg = got.expect("a dispatched search should answer");
        assert_eq!(msg.epoch, 1, "the result must carry the query generation");
        assert_eq!(msg.result.unwrap().matched, 2);

        let mut actors = actors;
        actors.shutdown();
    }

    #[test]
    fn the_worker_adopts_the_ui_generation_so_stale_results_are_detectable() {
        let (actors, rx) = start();
        let mut cmds = CmdList::new();
        cmds.push(Cmd::Search {
            query: "11-D-0704".into(),
            epoch: 42,
        });
        actors.dispatch(cmds);

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(AppEvent::Search(msg)) = rx.recv_timeout(Duration::from_millis(50)) {
                assert_eq!(msg.epoch, 42);
                let mut actors = actors;
                actors.shutdown();
                return;
            }
        }
        panic!("no result arrived");
    }

    #[test]
    fn shutdown_stops_every_worker_promptly() {
        let (mut actors, _rx) = start();
        let started = Instant::now();
        assert!(actors.shutdown(), "idle workers should stop cleanly");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "shutdown took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_quit_command_is_accepted_without_side_effects() {
        let (actors, _rx) = start();
        let mut cmds = CmdList::new();
        cmds.push(Cmd::Quit);
        actors.dispatch(cmds);
        let mut actors = actors;
        actors.shutdown();
    }

    #[test]
    fn the_demo_source_exercises_both_roots() {
        let src = fake_source_for_demo();
        let count = |dir: &str, files_only: bool| {
            let mut sink = crate::index::enumerate::CountingSink::default();
            let opts = crate::index::enumerate::ListOpts {
                files_only,
                ..Default::default()
            };
            src.list(
                std::path::Path::new(dir),
                &mut sink,
                &opts,
                &crate::util::cancel::CancelToken::never(),
            )
            .map(|_| sink.count)
        };

        // At the *configured* path, not `V:\`. Registering the flat share
        // anywhere else left the demo searching a directory the settings never
        // name, so it found nothing and looked like a broken index.
        assert_eq!(count(crate::config::CUSTPRO_PATH, true).unwrap(), 2_000);

        // The job root holds only folders, so it lists nothing under the usual
        // files-only options and its subdirectories only when a walk asks for
        // them. That difference is the whole reason the walk exists.
        assert_eq!(
            count("R:\\", true).unwrap(),
            0,
            "no loose files at the root"
        );
        assert!(
            count("R:\\", false).unwrap() >= 3,
            "but its subdirectories are there to descend into"
        );

        // Reachable *from that root*, rather than being a listing with no
        // entry in its parent - a shape no real share can have, and one a
        // recursive walk could never find.
        assert!(
            count("R:\\11d", true).unwrap() > 20,
            "the job folder is reachable"
        );
        assert!(
            count("R:\\archive\\2019\\odd name", true).unwrap() > 0,
            "and so is the folder no routing rule would have guessed"
        );
    }
}
