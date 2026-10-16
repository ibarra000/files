//! Starting, feeding and stopping the background threads.
//!
//! The thread set is fixed for the life of the process: search, verification
//! and live workers, one index actor per indexed share, the opener, and
//! optional update checker, history writer and hotkey threads, plus rayon's
//! pool for the matcher. Nothing is spawned per keystroke.
//!
//! All of them report into one channel, drained only by [`super::App`].

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

/// Generous so blocking sends rarely wait. Progress events, the only likely
/// way to fill it, are rate limited at the source.
const EVENT_CAPACITY: usize = 256;

/// Everything running in the background.
pub struct Actors {
    pub events: Events,
    pub backend: Arc<Backend>,
    pub verifier: Arc<Verifier>,
    search: WorkerHandle<SearchRequest>,
    verify: WorkerHandle<SearchRequest>,
    /// Queries the shares that are never indexed. See [`worker::spawn_live`].
    live: WorkerHandle<SearchRequest>,
    /// One per enabled, indexed mapping, in configuration order.
    indexes: Vec<IndexActor>,
    /// Opens what Enter chose. One long-lived thread, since assembling a
    /// document reads every page from the share.
    opener: open::worker::Opener,
    /// Checks the update folder periodically.
    ///
    /// `None` when no folder is configured.
    updates: Option<crate::update::check::Checker>,
    /// `None` when history is off or has no path. In-session recall still
    /// works.
    history: Option<history::Writer>,
    /// The global hotkey that summons the panel.
    ///
    /// `None` when disabled, unsupported, or the chord is already claimed;
    /// none of these may stop startup. Joinable, since a posted quit reaches
    /// its message pump.
    hotkey: Option<hotkey::HotkeyThread>,
    /// How the UI thread tells the hotkey thread which window to summon.
    ///
    /// Held here so the shell can set it after `Actors::start` returns, once
    /// the toolkit has created the window.
    pub panel: Arc<hotkey::Panel>,
}

impl Actors {
    /// Starts every worker and returns the channel they report on.
    ///
    /// `wake` is called after each event is posted, so a UI loop that sleeps
    /// sees background results.
    pub fn start(
        settings: Settings,
        source: Arc<dyn DirSource>,
        wake: Wake,
    ) -> std::io::Result<(Self, Receiver<AppEvent>)> {
        let (raw, rx) = bounded(EVENT_CAPACITY);
        let tx = Events::new(raw, wake);
        let store = Arc::new(IndexStore::for_routes(
            &settings.routes,
            crate::config::FOLDER_CACHE_CAPACITY,
        ));

        // Remove cache files that belong to no configured directory, or the
        // cache grows as roots are edited.
        if settings.persist
            && let Some(cache_dir) = &settings.cache_dir
        {
            // Every configured mapping (`all()`, not `enabled()`):
            // `gc_orphans` deletes anything that looks like ours, so a narrower
            // list would wipe the cache of a mapping disabled for this session
            // or of another configuration (env override, `--config`). Actors
            // write under exactly these paths, so sweep and writers agree.
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

        // A live share skips folders another enabled indexed mapping covers,
        // so a walked tree inside a live share is not searched twice.
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
        // One thread per enabled, indexed mapping, so a long walk or an
        // unreachable share's backoff cannot delay another share. Driven off
        // the store's slots, so a mapping with an empty path gets no actor.
        let log = Arc::new(crate::index::log::IndexLog::from_option(
            settings.index_log.as_deref(),
        ));
        // One permit pool shared by every actor caps concurrent scans per
        // machine rather than per share.
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
        // Merged documents cannot be deleted while a viewer has them open, so
        // the previous session's are collected at startup, before the opener
        // exists so the sweep cannot race a fresh merge.
        if let Some(cache_dir) = &settings.cache_dir {
            open::pdf::gc(cache_dir, open::worker::CACHE_LIFETIME);
        }
        let opener = open::worker::spawn(Arc::clone(&backend), tx.clone())?;

        // Best effort: if the checker cannot start, only update checks are
        // lost.
        let updates = crate::update::Source::of(&settings).and_then(|source| {
            crate::update::check::Checker::start(
                source,
                crate::update::Version::current(),
                settings.cache_dir.clone(),
                tx.clone(),
            )
            .ok()
        });

        // Best effort: a chord another program owns costs only the shortcut.
        // Started before the window exists; `Panel` connects them later, and a
        // chord pressed before then does nothing.
        let panel = hotkey::Panel::new();
        let hotkey = hotkey::spawn(settings.hotkey, tx.clone(), Arc::clone(&panel))?;

        // Best effort: failing to start the writer costs only recall next
        // session.
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

    /// Hides the panel once its exit transition has finished.
    ///
    /// Distinct from [`Cmd::DismissOverlay`], which is sent when the user asks
    /// to dismiss; this runs when the window may actually disappear.
    pub fn dismiss_overlay(&self) {
        if let Some(hotkey) = &self.hotkey {
            hotkey.hide();
        }
    }

    /// Brings the panel up, for the tray icon and for a second launch.
    ///
    /// Delegated to the hotkey thread, the only thread Windows lets take the
    /// foreground.
    pub fn summon_overlay(&self) {
        if let Some(hotkey) = &self.hotkey {
            hotkey.summon();
        }
    }

    /// Executes the commands a state transition produced.
    ///
    /// The only place that performs effects on behalf of the UI, which keeps
    /// the state transition free of side effects.
    pub fn dispatch(&self, cmds: CmdList) {
        // A dismissal in the same turn as an open means the panel is making
        // way for a viewer, not an Escape; the two need opposite foreground
        // handling (see `Summoner::hide`). Read from the list because
        // `Cmd::DismissOverlay` is a unit variant. The foreground is handed over
        // only when the panel goes away too: a topmost panel that stayed up
        // without the keyboard would float over the viewer.
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
                    // Only the targeted share; a full pass over a large share
                    // is too expensive to run on every share by default.
                    for index in &self.indexes {
                        if target.wants(index.mapping()) {
                            index.refresh(force);
                        }
                    }
                }
                Cmd::Open(request) => {
                    // Done here, on the UI thread, which still owns the
                    // foreground; Windows grants the handover only to the
                    // foreground process, and the open worker may finish long
                    // after the panel is gone. Skipped when the panel stays up,
                    // so it keeps the keyboard and the viewer opens behind it.
                    if handing_over {
                        crate::open::launch::allow_foreground_handover();
                    }
                    self.opener.request(request, &self.events);
                }
                Cmd::SaveViewer(name) => crate::config::write::save_viewer_async(
                    self.backend
                        .settings
                        .routes
                        .source()
                        .path()
                        .map(Path::to_path_buf),
                    name,
                    self.events.clone(),
                ),
                // Run inline: there is nothing to read or merge, so a worker
                // would only add latency. Hands over the foreground on the
                // same terms as an open.
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
                // Blocks the dispatch thread on purpose; a box shown from
                // elsewhere would appear behind whatever just opened.
                Cmd::Announce { title, detail } => crate::notify::tell(&title, &detail),
                Cmd::Copy(text) => clipboard::copy_async(text, self.events.clone()),
                Cmd::ReadClipboard => clipboard::read_async(self.events.clone()),
                Cmd::SaveHistory(entries) => {
                    if let Some(writer) = &self.history {
                        writer.store(entries);
                    }
                }
                // The hotkey thread owns whether the panel is up and reports
                // `HotkeyMsg::Dismissed`, which starts the exit animation; the
                // window is hidden later by `dismiss_overlay`.
                Cmd::DismissOverlay => {
                    if let Some(hotkey) = &self.hotkey {
                        hotkey.dismiss(handing_over);
                    }
                }
                // Handled by the shell, not a worker. Listed explicitly so an
                // unhandled new command is a compile error.
                Cmd::ToggleSettings | Cmd::Quit => {}
            }
        }
    }

    /// Asks the checker to look now rather than waiting for its timer.
    ///
    /// Does nothing when no update folder is configured.
    pub fn check_for_updates(&self) {
        if let Some(updates) = &self.updates {
            updates.check_now();
        }
    }

    /// Stops everything, with a bounded wait.
    ///
    /// Returns false when a thread had to be abandoned, which is expected when
    /// one is blocked in an SMB call. Index writes are temp-then-rename, so
    /// nothing is left half-written.
    pub fn shutdown(&mut self) -> bool {
        let budget = SHUTDOWN_JOIN_BUDGET;
        let mut clean = true;
        // First, because it is cheap.
        if let Some(hotkey) = &mut self.hotkey {
            clean &= hotkey.shutdown(budget);
        }
        clean &= self.search.shutdown(budget);
        clean &= self.verify.shutdown(budget);
        clean &= self.live.shutdown(budget);
        // Signal every index actor before joining any, so they share one
        // deadline instead of each getting the full budget.
        for index in &mut self.indexes {
            index.begin_shutdown();
        }
        let deadline = Instant::now() + budget;
        for index in &mut self.indexes {
            clean &= index.join(deadline);
        }
        clean &= self.opener.shutdown(budget);
        // Budgeted: at worst it is reading one small file and writes nothing.
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
    // Shaped like a real project folder so interesting cases can be tried by
    // hand:
    //
    //   * twenty pages, more than MAX_RESULTS, so the list stops at fifteen
    //     while the document opens whole;
    //   * a `.tif` page, which is not a member and must be skipped silently;
    //   * files that merely start with the base name, which belong to no
    //     document.
    let mut names: Vec<String> = vec!["AB-C-0704.pdf".into()];
    names.extend((1..=20).map(|i| format!("AB-C-0704_Page{i}.pdf")));
    names.push("AB-C-0704_Page21.tif".into());
    names.push("AB-C-0704 revision notes.pdf".into());
    names.push("AB-C-0704 notes.txt".into());
    // Registered as a tree at the configured roots so `--demo` exercises the
    // recursive walk. The nested names are ones no routing rule would guess.
    let nested: Vec<String> = [
        "archive\\2019\\odd name\\AB-3-0704 survey.pdf",
        "archive\\2019\\odd name\\AB-3-0704 notes.txt",
        "archive\\2020\\ab12-0704 spec.pdf",
        "ab12\\report.pdf",
        "ab12\\rev b\\report.pdf",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // The project folder is added as tree paths too, so it has an entry in its
    // parent and a recursive walk can reach it.
    let mut paths: Vec<String> = nested;
    paths.extend(names.iter().map(|n| format!("abc\\{n}")));
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();

    Arc::new(
        FakeDirSource::new()
            .with_synthetic(crate::config::FLAT_FIXTURE_PATH, 2_000)
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
            ..Settings::fixture()
        }
    }

    fn start() -> (Actors, Receiver<AppEvent>) {
        let source: Arc<dyn DirSource> = Arc::new(
            FakeDirSource::new()
                .with_dir(crate::config::FLAT_FIXTURE_PATH, &["alpha.pdf"])
                // A tree reachable from its root, since the tree share is
                // walked. The fixtures contain the typed search.
                .with_tree(
                    "R:\\",
                    &["abc\\AB-C-0704 one.pdf", "abc\\AB-C-0704 two.pdf"],
                ),
        );
        Actors::start(settings(), source, std::sync::Arc::new(|| {})).unwrap()
    }

    /// One actor per enabled indexed mapping, and none for anything else.
    ///
    /// With no flat mapping configured, no actor may probe an empty path and
    /// report `os error 3`.
    #[test]
    fn no_actor_is_started_for_a_share_that_is_not_configured() {
        let routes = crate::paths::Routes::single(
            "projects",
            std::path::PathBuf::from("R:\\"),
            crate::paths::MappingKind::Tree,
        );
        let settings = Settings {
            persist: false,
            ..Settings::with_routes(Arc::new(routes), |s| s)
        };
        let source: Arc<dyn DirSource> =
            Arc::new(FakeDirSource::new().with_tree("R:\\", &["abc\\one.pdf"]));

        let (mut actors, _rx) =
            Actors::start(settings, source, std::sync::Arc::new(|| {})).unwrap();
        assert_eq!(
            actors.indexes.len(),
            1,
            "a configuration with one share must start one actor"
        );
        actors.shutdown();
    }

    /// With no flat mapping, nothing reports itself unreachable.
    #[test]
    fn a_configuration_with_no_flat_share_never_reports_a_missing_one() {
        let routes = crate::paths::Routes::single(
            "projects",
            std::path::PathBuf::from("R:\\"),
            crate::paths::MappingKind::Tree,
        );
        let settings = Settings {
            persist: false,
            ..Settings::with_routes(Arc::new(routes), |s| s)
        };
        let source: Arc<dyn DirSource> =
            Arc::new(FakeDirSource::new().with_tree("R:\\", &["abc\\one.pdf"]));

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

        // The tree share is walked in the background, so wait for the index
        // before searching rather than asserting on a race.
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
            query: crate::search::query::Query::contains("AB-C-0704"),
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
            query: crate::search::query::Query::contains("AB-C-0704"),
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

        // At the configured path, so the demo searches what the settings name.
        assert_eq!(
            count(crate::config::FLAT_FIXTURE_PATH, true).unwrap(),
            2_000
        );

        // The tree root holds only folders: nothing under files-only options,
        // subdirectories when a walk asks for them.
        assert_eq!(
            count("R:\\", true).unwrap(),
            0,
            "no loose files at the root"
        );
        assert!(
            count("R:\\", false).unwrap() >= 3,
            "but its subdirectories are there to descend into"
        );

        // Reachable from that root, as on a real share.
        assert!(
            count("R:\\abc", true).unwrap() > 20,
            "the project folder is reachable"
        );
        assert!(
            count("R:\\archive\\2019\\odd name", true).unwrap() > 0,
            "and so is the folder no routing rule would have guessed"
        );
    }
}
