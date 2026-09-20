//! Starting, feeding and stopping the background threads.
//!
//! The thread population is fixed for the life of the process: one input
//! reader, one search worker, one verification worker, one index actor per
//! configured share, one change watcher, one hotkey listener where the
//! platform has global hotkeys and one is configured, plus rayon's pool for
//! the matcher.
//! Nothing is spawned per
//! keystroke, so thread growth is impossible by construction rather than by
//! discipline.
//!
//! All of them report into one channel, which the main loop is the only
//! receiver of.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, bounded};

use super::event::{AppEvent, Cmd, CmdList, Events, Wake};
use crate::clipboard;
use crate::config::{SHUTDOWN_JOIN_BUDGET, Settings};
use crate::history;
use crate::hotkey;
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
    pub events: Events,
    pub backend: Arc<Backend>,
    pub verifier: Arc<Verifier>,
    search: WorkerHandle<SearchRequest>,
    verify: WorkerHandle<SearchRequest>,
    /// Asks the shares that are never indexed. See [`worker::spawn_live`] for
    /// why it is not the verifier's thread.
    live: WorkerHandle<SearchRequest>,
    /// One per enabled, indexed mapping, in configuration order.
    ///
    /// Was a fixed `index` plus an optional `tree_index`, which is why a
    /// configuration naming ten shares indexed two of them - and why the flat
    /// actor was started even when no flat mapping existed, leaving it to
    /// probe an empty path and report `os error 3` forever.
    indexes: Vec<IndexActor>,
    /// Opens what Enter chose. One thread for the life of the process, like
    /// the rest: assembling a document reads every page off the share, which
    /// is far too much work to spawn a thread for per keypress.
    opener: open::worker::Opener,
    /// Looks at the update folder now and then.
    ///
    /// `None` when no folder is configured, which is what ships - so the
    /// whole feature costs nothing at all on a machine that has no such
    /// share, which is most of them until somebody sets one up.
    updates: Option<crate::update::check::Checker>,
    /// Absent when history is switched off, or when there is nowhere to put
    /// it. Recall still works within the session either way.
    history: Option<history::Writer>,
    /// The global hotkey that summons the panel.
    ///
    /// `None` when it is switched off, when the platform has none, or when the
    /// chord was already claimed - all three are ordinary, and none of them may
    /// stop the program starting.
    ///
    /// Unlike `_input` this one is genuinely joinable: it parks in a message
    /// pump that a posted quit reaches.
    hotkey: Option<hotkey::HotkeyThread>,
    /// How the drawing thread tells the hotkey thread which window to summon.
    ///
    /// Held here rather than passed straight through so the shell can reach it
    /// after `Actors::start` has returned - which it must, because the window
    /// does not exist until the toolkit has built one.
    pub panel: Arc<hotkey::Panel>,
}

impl Actors {
    /// Starts every background thread.
    /// Starts every worker, and hands back the channel they report on.
    ///
    /// `wake` is called after each event is posted. A driver that blocks on the
    /// receiver - which the terminal loop does - needs nothing and passes
    /// [`Events::headless`]'s no-op; a driver that owns its own event loop and
    /// sleeps has to be told, or a walk that finishes while the screen is idle
    /// is a walk nobody sees.
    pub fn start(
        settings: Settings,
        source: Arc<dyn DirSource>,
        wake: Wake,
    ) -> std::io::Result<(Self, Receiver<AppEvent>)> {
        let (raw, rx) = bounded(EVENT_CAPACITY);
        let tx = Events::new(raw, wake);
        let store = Arc::new(IndexStore::for_routes(
            &settings.routes,
            crate::config::JOB_CACHE_CAPACITY,
        ));

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
            // `all()` rather than `enabled()`: a mapping switched off for a
            // session keeps its cache, so switching it back on is not a cold
            // start. The derived roots this used to append no longer exist -
            // the actors now write under exactly the mapping paths listed
            // here, so the sweep and the writers cannot disagree.
            let live: Vec<_> = settings
                .routes
                .all()
                .iter()
                .map(|m| m.path.clone())
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| crate::index::persist::MappingKey::of(p.as_path()))
                .collect();
            crate::index::persist::gc_orphans(cache_dir, &live);
        }

        // A live share skips any folder another enabled mapping already
        // covers, which is what makes a walked tree *inside* a live share a
        // legal configuration rather than a way to see every file twice.
        let live: Vec<Arc<crate::search::live::LiveShare>> = settings
            .routes
            .live()
            .map(|m| {
                let excluded: Vec<std::path::PathBuf> = settings
                    .routes
                    .enabled()
                    .filter(|o| o.id != m.id && o.kind.is_indexed())
                    .map(|o| o.path.clone())
                    .collect();
                Arc::new(crate::search::live::LiveShare::new(
                    m.id,
                    m.path.clone(),
                    m.depth,
                    excluded,
                    Arc::clone(&source),
                ))
            })
            .collect();

        let backend = Arc::new(Backend {
            settings: settings.clone(),
            store: Arc::clone(&store),
            source: Arc::clone(&source),
            live,
        });

        let verifier = Arc::new(Verifier::for_routes(
            Arc::clone(&source),
            &settings.routes,
            settings.server_filter,
        ));

        let search = worker::spawn_search(Arc::clone(&backend), tx.clone())?;
        let verify = worker::spawn_verify(Arc::clone(&backend), Arc::clone(&verifier), tx.clone())?;
        let live = worker::spawn_live(Arc::clone(&backend), tx.clone())?;
        // One actor per enabled, indexed mapping. One thread each rather than
        // one for several: a walk runs for minutes, and sharing would mean one
        // share's freshness queued behind another - or a five-minute backoff
        // on an unreachable share delaying a healthy one.
        //
        // Driven off the store's own slots, so a mapping with an empty path is
        // skipped by the same predicate a search uses. That is the whole of
        // the `os error 3` fix: there is no longer an actor without a share.
        let log = Arc::new(crate::index::log::IndexLog::from_option(
            settings.index_log.as_deref(),
        ));
        // One set of permits shared by every actor, so the cap is on the
        // machine rather than on each share independently.
        let permits = Arc::new(crate::index::permit::WalkPermits::new(
            settings.max_concurrent_scans,
        ));
        let mut indexes = Vec::new();
        for slot in store.indexed() {
            indexes.push(actor::spawn(
                IndexContext::new(
                    settings.clone(),
                    slot.id(),
                    Arc::clone(&store),
                    Arc::clone(&source),
                )
                .with_log(Arc::clone(&log))
                .with_permits(Arc::clone(&permits))
                .with_live_updates(&settings),
                tx.clone(),
            )?);
        }
        // Merged documents cannot be deleted once a viewer has them open, so
        // last session's are collected at the start of this one - the same
        // arrangement the index cache uses just above. Swept *before* the
        // opener exists, so the sweep cannot race a merge it just wrote.
        if let Some(cache_dir) = &settings.cache_dir {
            open::pdf::gc(cache_dir, open::worker::CACHE_LIFETIME);
        }
        let opener = open::worker::spawn(Arc::clone(&backend), tx.clone())?;

        // Only when there is somewhere to look. Best effort beyond that, like
        // the history writer: a thread that would not start must cost the
        // checking and never the program.
        let updates = settings.update_from.as_ref().and_then(|folder| {
            crate::update::check::Checker::start(
                folder.clone(),
                crate::update::Version::current(),
                tx.clone(),
            )
            .ok()
        });

        // Best effort, like the history writer: a chord another program owns
        // must cost the shortcut, never the program.
        //
        // Started before there is a window to summon. `Panel` is how the one
        // that eventually exists reaches this thread, and a chord pressed
        // before then does nothing - which is the right answer for the first
        // few hundred milliseconds of a process's life.
        let panel = hotkey::Panel::new();
        let hotkey = hotkey::spawn(settings.hotkey, tx.clone(), Arc::clone(&panel))?;

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
                live,
                indexes,
                opener,
                updates,
                history,
                hotkey,
                panel,
            },
            rx,
        ))
    }

    /// Executes the commands a state transition produced.
    ///
    /// The only place in the program that touches a channel on behalf of the
    /// UI, which keeps the state transition itself free of side effects.
    /// Puts the panel away.
    ///
    /// Separate from [`Cmd::DismissOverlay`], and deliberately so: the command
    /// is sent when the user asks to dismiss, and this is called when the exit
    /// transition has finished playing. One is an intention and the other is
    /// the moment the window may safely disappear, and collapsing them is how
    /// a transition comes to be written and never seen.
    pub fn dismiss_overlay(&self) {
        if let Some(hotkey) = &self.hotkey {
            hotkey.hide();
        }
    }

    /// Brings the panel up, for the tray icon and for a second launch.
    ///
    /// Goes to the hotkey thread rather than being done here, because showing
    /// the panel means taking the foreground and that is the one thread Windows
    /// will accept it from.
    pub fn summon_overlay(&self) {
        if let Some(hotkey) = &self.hotkey {
            hotkey.summon();
        }
    }

    pub fn dispatch(&self, cmds: CmdList) {
        // A dismissal in the same turn as an open is the panel getting out of
        // a viewer's way, not somebody pressing Escape. The two want opposite
        // things from the foreground and are otherwise indistinguishable by
        // the time they reach the hotkey thread - see `Summoner::hide`.
        //
        // Read off the list rather than carried on the command, because
        // `Cmd::DismissOverlay` is a unit variant that a dozen tests match by
        // equality, and `open_selection` pushes the open first.
        //
        // Both halves are required, and the second one is new. The panel is
        // topmost: if it stays visible and gives the foreground away anyway,
        // it ends up floating over the viewer without the keyboard, and what
        // the user types goes somewhere they are not looking. So the foreground
        // is handed over only when the panel is going away with it. Escape has
        // no `Cmd::Open` and still restores the window behind, as before.
        let handing_over = cmds.iter().any(|cmd| matches!(cmd, Cmd::Open(_)))
            && cmds.iter().any(|cmd| matches!(cmd, Cmd::DismissOverlay));

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
                Cmd::Live { query, epoch } => {
                    self.live
                        .submit_generation(epoch, SearchRequest { query, epoch });
                }
                Cmd::RefreshIndex { target, force } => {
                    // Only the shares asked for. "All" is still a keystroke
                    // away, but it is no longer the only thing F5 can mean:
                    // a full pass over one large share is expensive enough
                    // that doing it to every share by default is what put a
                    // few hundred clients on the server at once.
                    for index in &self.indexes {
                        if target.wants(index.mapping()) {
                            index.refresh(force);
                        }
                    }
                }
                Cmd::Open(request) => {
                    // Here, and not on the worker that does the launching.
                    // Windows grants this only to the process that currently
                    // owns the foreground, and by the time the open worker has
                    // merged a document off a share, the panel is long gone.
                    // `dispatch` runs on the thread that draws the panel, which
                    // still has it.
                    //
                    // Not given away when the panel is staying up: it keeps the
                    // keyboard so the next code can be typed straight away, and
                    // the viewer comes up behind it. Escape brings it forward.
                    if handing_over {
                        crate::open::launch::allow_foreground_handover();
                    }
                    self.opener.request(request, &self.events);
                }
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
                Cmd::SaveSetting { edit, label } => crate::config::write::save_async(
                    self.backend
                        .settings
                        .routes
                        .source()
                        .path()
                        .map(Path::to_path_buf),
                    vec![edit],
                    self.events.clone(),
                    move |outcome| {
                        AppEvent::Open(match outcome {
                            Ok(()) => crate::app::event::OpenMsg::SettingSaved { label },
                            Err(detail) => {
                                crate::app::event::OpenMsg::SettingSaveFailed { label, detail }
                            }
                        })
                    },
                ),
                // On the spot rather than on a worker. Explorer either
                // starts or it does not; there is no share to read and
                // nothing to merge, so the round trip through a thread would
                // buy a frame of latency and no safety.
                //
                // It hands the foreground over on the same terms an open
                // does: what comes up is a window the user asked for and
                // wants in front of them.
                Cmd::Reveal(path) => {
                    if handing_over {
                        crate::open::launch::allow_foreground_handover();
                    }
                    if let Err(e) = crate::open::launch::reveal(&path) {
                        let _ =
                            self.events
                                .send(AppEvent::Open(crate::app::event::OpenMsg::Failed {
                                    path,
                                    detail: e.detail(),
                                }));
                    }
                }
                // Blocking, on the dispatch thread, and deliberately: the
                // alternative is a box that appears behind whatever just
                // opened, which is a box nobody sees.
                Cmd::Announce { title, detail } => crate::notify::tell(&title, &detail),
                Cmd::Copy(text) => clipboard::copy_async(text, self.events.clone()),
                Cmd::ReadClipboard => clipboard::read_async(self.events.clone()),
                Cmd::SaveHistory(entries) => {
                    if let Some(writer) = &self.history {
                        writer.store(entries);
                    }
                }
                // Starts the exit. The hotkey thread owns whether the panel is
                // up and reports back as `HotkeyMsg::Dismissed`, which is what
                // sets the animation going; the window itself comes off the
                // screen later, from `dismiss_overlay`. One thread hop of
                // latency buys the impossibility of the two disagreeing about
                // whether the panel is on screen.
                Cmd::DismissOverlay => {
                    if let Some(hotkey) = &self.hotkey {
                        hotkey.dismiss(handing_over);
                    }
                }
                // Both are for whoever is drawing, not for a worker. Named
                // rather than wildcarded, so a command nothing handles is a
                // compile error here instead of a keystroke that does nothing.
                Cmd::ToggleSettings | Cmd::Quit => {}
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
    /// Asks the checker to look now, rather than waiting for its timer.
    ///
    /// Does nothing when no update folder is configured, which is the same
    /// answer the button gives: there is nowhere to look.
    pub fn check_for_updates(&self) {
        if let Some(updates) = &self.updates {
            updates.check_now();
        }
    }

    pub fn shutdown(&mut self) -> bool {
        let budget = SHUTDOWN_JOIN_BUDGET;
        let mut clean = true;
        // First: it costs microseconds, and it is the only thread that can
        // still put the terminal back where it was found.
        if let Some(hotkey) = &mut self.hotkey {
            clean &= hotkey.shutdown(budget);
        }
        clean &= self.search.shutdown(budget);
        clean &= self.verify.shutdown(budget);
        clean &= self.live.shutdown(budget);
        // Every index actor is told to stop before any of them is waited on,
        // and they share one deadline. Signalling and joining one at a time
        // would make the budget per-thread, so quitting with ten shares
        // configured could take ten times as long as it promises.
        for index in &mut self.indexes {
            index.begin_shutdown();
        }
        let deadline = Instant::now() + budget;
        for index in &mut self.indexes {
            clean &= index.join(deadline);
        }
        clean &= self.opener.shutdown(budget);
        // Budgeted: the worst this can be waiting on is one read of a small
        // file, and there is nothing half-written for an abandoned one to
        // leave behind.
        if let Some(updates) = &mut self.updates {
            clean &= updates.shutdown(budget);
        }
        if let Some(writer) = &mut self.history {
            writer.shutdown();
        }
        clean
    }
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
        Actors::start(settings(), source, std::sync::Arc::new(|| {})).unwrap()
    }

    /// One actor per enabled indexed mapping, and none for anything else.
    ///
    /// The direct regression test for the reported `os error 3`. A flat actor
    /// used to be started unconditionally; with no flat mapping configured its
    /// directory was the derived `custpro_path`, which is an empty `PathBuf`,
    /// so it probed `""`, got `ERROR_PATH_NOT_FOUND`, and pinned
    /// `Health::Unreachable` forever with nothing named in the message.
    #[test]
    fn no_actor_is_started_for_a_share_that_is_not_configured() {
        let routes = crate::paths::Routes::single(
            "jobs",
            std::path::PathBuf::from("R:\\"),
            crate::paths::MappingKind::Tree,
        );
        let settings = Settings {
            persist: false,
            ..Settings::with_routes(Arc::new(routes), |s| s)
        };
        let source: Arc<dyn DirSource> =
            Arc::new(FakeDirSource::new().with_tree("R:\\", &["11d\\one.pdf"]));

        let (mut actors, _rx) =
            Actors::start(settings, source, std::sync::Arc::new(|| {})).unwrap();
        assert_eq!(
            actors.indexes.len(),
            1,
            "a configuration with one share must start one actor"
        );
        actors.shutdown();
    }

    /// And with no flat mapping, nothing ever reports itself unreachable.
    #[test]
    fn a_configuration_with_no_flat_share_never_reports_a_missing_one() {
        let routes = crate::paths::Routes::single(
            "jobs",
            std::path::PathBuf::from("R:\\"),
            crate::paths::MappingKind::Tree,
        );
        let settings = Settings {
            persist: false,
            ..Settings::with_routes(Arc::new(routes), |s| s)
        };
        let source: Arc<dyn DirSource> =
            Arc::new(FakeDirSource::new().with_tree("R:\\", &["11d\\one.pdf"]));

        let (mut actors, _rx) =
            Actors::start(settings, source, std::sync::Arc::new(|| {})).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            for slot in actors.backend.store.slots() {
                assert!(
                    !slot.status().health.is_unreachable(),
                    "{} reported unreachable: {:?}",
                    slot.name(),
                    slot.status().health
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        actors.shutdown();
    }

    /// Ten shares, which is the configuration that prompted this work. Each
    /// gets its own actor and its own slot; the store used to hold two.
    #[test]
    fn ten_configured_shares_each_get_an_actor_and_a_slot() {
        let mut mappings = Vec::new();
        let mut src = FakeDirSource::new();
        for i in 0..10u16 {
            let path = format!("X:\\share{i}");
            src = src.with_tree(&path, &["a\\one.pdf"]);
            mappings.push(crate::paths::Mapping {
                id: crate::paths::MappingId(i),
                name: format!("share{i}").into(),
                path: path.into(),
                kind: crate::paths::MappingKind::Tree,
                enabled: true,
                refresh: Default::default(),
                depth: crate::config::DEFAULT_LIVE_DEPTH,
            });
        }
        let routes = crate::paths::Routes::new(mappings, crate::paths::ConfigSource::BuiltIn);
        let settings = Settings {
            persist: false,
            ..Settings::with_routes(Arc::new(routes), |s| s)
        };

        let (mut actors, _rx) = Actors::start(settings, Arc::new(src), Arc::new(|| {})).unwrap();
        assert_eq!(actors.indexes.len(), 10, "every share is indexed");
        assert_eq!(actors.backend.store.slots().len(), 10);
        assert_eq!(actors.backend.store.indexed().count(), 10);
        actors.shutdown();
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
        while Instant::now() < deadline
            && actors.backend.store.first_tree_slot().as_tree().is_none()
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            actors.backend.store.first_tree_slot().as_tree().is_some(),
            "the tree should have been walked"
        );

        let mut cmds = CmdList::new();
        cmds.push(Cmd::Search {
            query: crate::search::query::Query::contains("11-D-0704"),
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
            query: crate::search::query::Query::contains("11-D-0704"),
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
