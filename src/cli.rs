//! Command-line argument parsing, shared by `files` and `files-cli`.
//!
//! Hand-rolled to avoid a parser dependency and keep startup trivial.

use std::path::PathBuf;

use crate::config::file::ConfigError;
use crate::config::write::SettingKey;
use crate::config::{ConfigChoice, EnumStrategy, MatcherKind, Settings, ViewerKind};
use crate::view::settings::PageId;

/// How a `--bench --walk` run is bounded.
///
/// Separate from `walk::WalkOpts` so unset values fall back to that type's
/// defaults instead of being restated here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkArgs {
    pub concurrency: Option<usize>,
    pub max_depth: Option<u16>,
}

/// What the program was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Run the search panel (the default).
    Gui,
    /// Run only the settings window.
    ///
    /// The panel opens settings by starting a copy of itself in this mode, so
    /// the window is independent: resizable, in the taskbar, and not under the
    /// always-on-top panel. Without a running panel it still edits the config
    /// file, and reports the actions that need the panel as unavailable.
    Settings {
        /// Page to open on; `None` reopens the last page shown.
        page: Option<PageId>,
        /// Settings pinned by the panel's own command-line flags (its
        /// `Settings::cli_pinned`), so the window does not offer to save
        /// values a flag overrides. See `config::Pin`.
        pinned: u32,
    },
    /// Fast, read-only capability report.
    Doctor,
    /// Timing comparison across enumeration strategies.
    Bench {
        query: Option<String>,
        allow_write: bool,
        /// Walk the tree recursively and report its shape instead of timing a
        /// single directory.
        walk: Option<WalkArgs>,
    },
    /// Validate the configuration and print the routing table, without
    /// touching the network.
    CheckConfig {
        query: Option<String>,
    },
    /// Finish an update: wait for the panel to exit, install, start it again.
    ///
    /// Internal: `files.exe` starts a staged copy of `files-cli.exe` this way,
    /// because an installer cannot replace the executable driving it. See
    /// `crate::update::apply`.
    ApplyUpdate {
        msi: PathBuf,
        wait_pid: u32,
        relaunch: Option<PathBuf>,
    },
    Help,
    Version,
}

/// Command-line overrides, applied on top of the file and the environment.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub enum_strategy: Option<EnumStrategy>,
    pub matcher: Option<MatcherKind>,
    pub server_filter: Option<bool>,
    pub persist: Option<bool>,
    pub index_log: Option<PathBuf>,
    /// Unvalidated; valid names depend on the configuration.
    pub viewer: Option<String>,
    pub pdf_viewer: Option<PathBuf>,
    pub hotkey: Option<crate::hotkey::spec::HotkeySpec>,
}

#[derive(Debug, Clone)]
pub struct Args {
    pub mode: Mode,
    pub config: ConfigChoice,
    pub overrides: Overrides,
    /// Use an in-memory fake source instead of the real drives.
    pub demo: bool,
}

/// Applies an override and pins its key to the session.
///
/// Bundled so a new override cannot be applied without also being pinned.
fn pinned<T>(
    s: &mut Settings,
    key: SettingKey,
    value: Option<T>,
    apply: impl FnOnce(&mut Settings, T),
) {
    if let Some(v) = value {
        apply(s, v);
        s.pin_to_session(key);
    }
}

/// Error message for a `--viewer` value that matches no viewer.
fn unknown_viewer(name: &str, viewers: &crate::config::Viewers) -> String {
    let mut expected = String::from("\"auto\", \"pdf\"");
    for n in viewers.names() {
        expected.push_str(&format!(", {n:?}"));
    }
    format!(
        "unknown --viewer value {name:?} (expected {expected}, or a [[viewer]] added to the configuration)"
    )
}

impl Args {
    /// Resolves settings: shipped defaults, then the config file, then the
    /// environment, then these flags.
    pub fn settings(&self) -> Result<Settings, Vec<ConfigError>> {
        let mut s = Settings::load(&self.config)?;
        if let Some(v) = self.overrides.enum_strategy {
            s.enum_strategy = v;
        }
        if let Some(v) = self.overrides.matcher {
            s.matcher = v;
        }
        if let Some(v) = self.overrides.server_filter {
            s.server_filter = v;
        }
        if let Some(v) = self.overrides.persist {
            s.persist = v;
        }
        // The overrides above are not writable from the settings window. Those
        // below are, so each must be pinned; otherwise the window would save a
        // value that the flag overrides.
        let viewer = match self.overrides.viewer.as_deref() {
            Some(name) => Some(ViewerKind::parse(name, &s.viewers).ok_or_else(|| {
                vec![ConfigError {
                    path: PathBuf::from("--viewer"),
                    loc: None,
                    entry: None,
                    rule: None,
                    message: unknown_viewer(name, &s.viewers),
                    snippet: None,
                }]
            })?),
            None => None,
        };
        pinned(&mut s, SettingKey::Viewer, viewer, |s, v| s.viewer = v);
        pinned(
            &mut s,
            SettingKey::PdfViewer,
            self.overrides.pdf_viewer.clone(),
            |s, v| s.pdf_viewer = Some(v),
        );
        pinned(&mut s, SettingKey::Hotkey, self.overrides.hotkey, |s, v| {
            s.hotkey = v
        });
        pinned(
            &mut s,
            SettingKey::IndexLog,
            self.overrides.index_log.clone(),
            |s, v| s.index_log = Some(v),
        );
        // In the settings window, also merge in the panel's pins; both sets
        // apply.
        if let Mode::Settings { pinned, .. } = self.mode {
            s.cli_pinned |= pinned;
        }
        Ok(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgError(pub String);

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub const HELP: &str = "\
files - nfs & smb indexer

USAGE:
    files [OPTIONS]                 the search panel (tray application)
    files --settings                the settings window
    files-cli --doctor
    files-cli --check-config [--query <TEXT>]
    files-cli --bench [--query <TEXT>] [--allow-write]
    files-cli --bench --walk [--concurrency <N>] [--max-depth <N>]

MODES:
    (none)              run the search panel
    --settings          open the settings window. Works without a running
                        panel; every setting still saves
    --doctor            report drive type, SMB dialect and capabilities
                        (fast and read-only)
    --bench             time each enumeration strategy against the
                        configured drives and check that they agree
                        (slow: reads each drive several times)
    --bench --walk      walk each configured drive recursively and report
                        directory and file counts, depth and duration
                        (read-only; can take minutes on a large drive)

OPTIONS:
    --config <PATH>     read this configuration file instead of the default
    --check-config      validate the configuration, print the drives and
                        exit, without touching the network
    --no-config         ignore the configuration file and use the built-in
                        defaults
    --query <TEXT>      search input for --bench or --check-config
    --allow-write       let --bench create one temporary file, to confirm
                        directory timestamps update on this server
    --concurrency <N>   directories --bench --walk reads at once (default 8,
                        max 64)
    --max-depth <N>     maximum depth for --bench --walk (default 32)
    --enum <STRATEGY>   handle | findfirst | std        (default: handle)
    --matcher <KIND>    simd | naive                    (default: simd)
    --server-filter <on|off>
                        send the search pattern to the file server
                        (default: off until --bench confirms it is safe)
    --viewer <KIND>     auto | pdf | <a [[viewer]] name> (default: auto)
                        auto opens each file in its default app. pdf
                        combines a base name's pages into one document;
                        types a [[viewer]] claims go to that viewer instead. A
                        [[viewer]] name opens the selected file with that
                        program. F2 switches at run time; this flag pins it
    --pdf-viewer <PATH> open combined PDFs with this program instead of the
                        default PDF app
    --hotkey <CHORD>    global hotkey that shows the panel, or \"off\" to
                        disable it. ctrl/alt/shift/win plus a letter, digit,
                        f1-f24 or space (default: ctrl+shift+space). The
                        Copilot key sends shift+win+f23
    --apply-update      finish an update: wait for the running panel to exit,
    --msi <PATH>        install <PATH>, then start --relaunch. Started by the
    --wait-pid <PID>    panel from a staging folder, because an installer
    --relaunch <PATH>   cannot replace the program running it. Internal
    --no-persist        do not read or write the on-disk index
    --index-log <PATH>  append one line per index scheduling decision: the
                        trigger, the directory stamp, whether it rebuilt,
                        and the next check (off by default)
    --demo              run against synthetic data, with no drives
    -h, --help          show this help
    -V, --version       show the version

CONFIGURATION:
    %APPDATA%\\files\\config.toml, written from a commented template on
    first run. Add a [[mapping]] for each drive to search, then validate
    the file with files-cli --check-config.

ENVIRONMENT:
    FILES_FS_STRATEGY, FILES_MATCHER, FILES_SERVER_FILTER, FILES_PERSIST,
    FILES_CACHE_DIR, FILES_INDEX_LOG, FILES_VIEWER, FILES_PDF_VIEWER,
    FILES_HISTORY, FILES_HOTKEY, FILES_THEME, FILES_STALE_NOTICES,
    FILES_LIVE_UPDATES, FILES_HIDE_EXTENSIONS, FILES_HIDE_SYSTEM_FILES,
    FILES_PDF_READ_ONLY, FILES_MAX_CONCURRENT_SCANS,
    FILES_UPDATE_FROM, FILES_DEV_MODE, FILES_BACKDROP,
    FILES_RESULT_LAYOUT, FILES_HIDE_ON_BLUR, FILES_HIDE_AFTER_OPENING,
    FILES_HIDE_ON_ESCAPE, FILES_DOCK, FILES_COLUMNS,
    FILES_CHECK_FOR_UPDATES, FILES_UPDATE_GITHUB

    These outrank the configuration file. A setting held by one applies for
    the session only, and the settings window names the variable.

    FILES_PROBE_INTERVAL, FILES_RESCAN_FLOOR
                        seconds between change checks, and the longest gap
                        between full rescans. Lower both to reproduce a
                        freshness problem quickly
";

/// Parses arguments over a base of environment-derived settings.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Args, ArgError> {
    let mut mode = Mode::Gui;
    let mut config = ConfigChoice::Default;
    let mut overrides = Overrides::default();
    let mut query = None;
    let mut allow_write = false;
    let mut walk: Option<WalkArgs> = None;
    let mut demo = false;
    let mut check_config = false;
    let mut apply_update = false;
    let mut msi: Option<PathBuf> = None;
    let mut wait_pid: Option<u32> = None;
    let mut relaunch: Option<PathBuf> = None;
    let mut settings_window = false;
    let mut page: Option<PageId> = None;
    let mut pinned = 0u32;

    let mut it = args.into_iter().peekable();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| -> Result<String, ArgError> {
            it.next()
                .ok_or_else(|| ArgError(format!("{name} needs a value")))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                return Ok(Args {
                    mode: Mode::Help,
                    config,
                    overrides,
                    demo,
                });
            }
            "-V" | "--version" => {
                return Ok(Args {
                    mode: Mode::Version,
                    config,
                    overrides,
                    demo,
                });
            }
            "--doctor" => mode = Mode::Doctor,
            "--bench" => {
                mode = Mode::Bench {
                    query: None,
                    allow_write: false,
                    walk: None,
                }
            }
            "--walk" => walk = Some(WalkArgs::default()),
            "--concurrency" => {
                let n = value("--concurrency")?;
                let n: usize = n
                    .parse()
                    .map_err(|_| ArgError(format!("--concurrency expects a number, got {n:?}")))?;
                walk.get_or_insert_with(WalkArgs::default).concurrency = Some(n.clamp(1, 64));
            }
            "--max-depth" => {
                let d = value("--max-depth")?;
                let d: u16 = d
                    .parse()
                    .map_err(|_| ArgError(format!("--max-depth expects a number, got {d:?}")))?;
                walk.get_or_insert_with(WalkArgs::default).max_depth = Some(d.max(1));
            }
            "--settings" => settings_window = true,
            "--page" => {
                let raw = value("--page")?;
                page = Some(
                    PageId::ALL
                        .into_iter()
                        .find(|p| p.slug() == raw)
                        .ok_or_else(|| ArgError(format!("there is no {raw:?} page")))?,
                );
            }
            "--pinned" => {
                let raw = value("--pinned")?;
                pinned = raw
                    .parse::<u32>()
                    .map_err(|_| ArgError(format!("--pinned wants a number, not {raw:?}")))?;
            }
            "--check-config" => check_config = true,
            "--apply-update" => apply_update = true,
            "--msi" => msi = Some(PathBuf::from(value("--msi")?)),
            "--wait-pid" => {
                let raw = value("--wait-pid")?;
                wait_pid =
                    Some(raw.parse::<u32>().map_err(|_| {
                        ArgError(format!("--wait-pid wants a number, not {raw:?}"))
                    })?);
            }
            "--relaunch" => relaunch = Some(PathBuf::from(value("--relaunch")?)),
            "--config" => config = ConfigChoice::Explicit(PathBuf::from(value("--config")?)),
            "--no-config" => config = ConfigChoice::None,
            "--allow-write" => allow_write = true,
            "--demo" => demo = true,
            "--no-persist" => overrides.persist = Some(false),
            "--viewer" => {
                // Validated in `settings`, once the configured viewers are known.
                overrides.viewer = Some(value("--viewer")?);
            }
            "--pdf-viewer" => {
                overrides.pdf_viewer = Some(PathBuf::from(value("--pdf-viewer")?));
            }
            "--hotkey" => {
                let v = value("--hotkey")?;
                overrides.hotkey = Some(
                    crate::hotkey::spec::parse(&v)
                        .map_err(|e| ArgError(format!("--hotkey: {}", e.detail())))?,
                );
            }
            "--index-log" => {
                overrides.index_log = Some(PathBuf::from(value("--index-log")?));
            }
            "--query" => query = Some(value("--query")?),
            "--enum" => {
                let v = value("--enum")?;
                overrides.enum_strategy = Some(
                    EnumStrategy::parse(&v)
                        .ok_or_else(|| ArgError(format!("unknown --enum value: {v}")))?,
                );
            }
            "--matcher" => {
                let v = value("--matcher")?;
                overrides.matcher = Some(
                    MatcherKind::parse(&v)
                        .ok_or_else(|| ArgError(format!("unknown --matcher value: {v}")))?,
                );
            }
            "--server-filter" => {
                let v = value("--server-filter")?;
                overrides.server_filter = Some(match v.to_ascii_lowercase().as_str() {
                    "on" | "1" | "true" | "yes" => true,
                    "off" | "0" | "false" | "no" => false,
                    other => {
                        return Err(ArgError(format!(
                            "--server-filter expects on/off, got {other}"
                        )));
                    }
                });
            }
            other => return Err(ArgError(format!("unrecognised argument: {other}"))),
        }
    }

    // Takes precedence over every other mode, including `--check-config`:
    // it is issued by the exiting app, not chosen by a user.
    if apply_update {
        let msi = msi.ok_or_else(|| ArgError("--apply-update needs --msi".into()))?;
        let wait_pid =
            wait_pid.ok_or_else(|| ArgError("--apply-update needs --wait-pid".into()))?;
        return Ok(Args {
            mode: Mode::ApplyUpdate {
                msi,
                wait_pid,
                relaunch,
            },
            config,
            overrides,
            demo,
        });
    }

    // Below `--apply-update`, above the text modes: it is issued by the
    // running panel.
    if settings_window {
        return Ok(Args {
            mode: Mode::Settings { page, pinned },
            config,
            overrides,
            demo,
        });
    }

    // `--check-config` wins over a mode, so `--check-config --query X` shows
    // where X would go rather than benchmarking.
    if check_config {
        mode = Mode::CheckConfig { query };
    } else if let Mode::Bench { .. } = mode {
        mode = Mode::Bench {
            query,
            allow_write,
            walk,
        };
    }
    Ok(Args {
        mode,
        config,
        overrides,
        demo,
    })
}

/// Parses the real process arguments, skipping `argv[0]`.
pub fn parse_env() -> Result<Args, ArgError> {
    parse(std::env::args().skip(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn args(list: &[&str]) -> Result<Args, ArgError> {
        parse(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_arguments_runs_the_interactive_search() {
        assert_eq!(args(&[]).unwrap().mode, Mode::Gui);
    }

    /// `--no-config` isolates the test from the machine's config file.
    #[test]
    fn a_flag_that_overrides_a_writable_setting_pins_it_for_the_session() {
        let a = args(&[
            "--no-config",
            "--viewer",
            "pdf",
            "--pdf-viewer",
            r"C:\viewer.exe",
            "--hotkey",
            "ctrl+alt+j",
            "--index-log",
            r"C:\index.log",
        ])
        .unwrap();
        let s = a.settings().expect("these flags are all valid");

        for key in [
            SettingKey::Viewer,
            SettingKey::PdfViewer,
            SettingKey::Hotkey,
            SettingKey::IndexLog,
        ] {
            assert_eq!(
                s.pin(key),
                Some(crate::config::Pin::CommandLine),
                "{} was overridden by a flag and did not say so",
                key.name()
            );
        }
    }

    #[test]
    fn a_setting_no_flag_touched_is_not_pinned_to_the_command_line() {
        let s = args(&["--no-config", "--viewer", "pdf"])
            .unwrap()
            .settings()
            .unwrap();
        assert_ne!(
            s.pin(SettingKey::Theme),
            Some(crate::config::Pin::CommandLine)
        );
    }

    #[test]
    fn recognises_each_mode() {
        assert_eq!(args(&["--doctor"]).unwrap().mode, Mode::Doctor);
        assert_eq!(args(&["--help"]).unwrap().mode, Mode::Help);
        assert_eq!(args(&["-h"]).unwrap().mode, Mode::Help);
        assert_eq!(args(&["-V"]).unwrap().mode, Mode::Version);
        assert_eq!(
            args(&["--bench"]).unwrap().mode,
            Mode::Bench {
                query: None,
                allow_write: false,
                walk: None
            }
        );
    }

    #[test]
    fn bench_collects_its_query_and_write_permission() {
        let a = args(&["--bench", "--query", "p12345", "--allow-write"]).unwrap();
        assert_eq!(
            a.mode,
            Mode::Bench {
                query: Some("p12345".into()),
                allow_write: true,
                walk: None
            }
        );
    }

    /// Writing is opt-in: the benchmark otherwise only reads the share.
    #[test]
    fn bench_does_not_write_unless_asked() {
        match args(&["--bench"]).unwrap().mode {
            Mode::Bench { allow_write, .. } => assert!(!allow_write),
            other => panic!("expected bench, got {other:?}"),
        }
    }

    #[test]
    fn selects_an_enumeration_strategy() {
        assert_eq!(
            args(&["--enum", "std"]).unwrap().overrides.enum_strategy,
            Some(EnumStrategy::StdReadDir)
        );
        assert_eq!(
            args(&["--enum", "findfirst"])
                .unwrap()
                .overrides
                .enum_strategy,
            Some(EnumStrategy::FindFirstEx)
        );
        assert_eq!(args(&[]).unwrap().overrides.enum_strategy, None);
    }

    #[test]
    fn selects_a_matcher() {
        assert_eq!(
            args(&["--matcher", "naive"]).unwrap().overrides.matcher,
            Some(MatcherKind::Naive)
        );
    }

    #[test]
    fn toggles_the_server_filter() {
        assert_eq!(
            args(&["--server-filter", "on"])
                .unwrap()
                .overrides
                .server_filter,
            Some(true)
        );
        assert_eq!(
            args(&["--server-filter", "off"])
                .unwrap()
                .overrides
                .server_filter,
            Some(false)
        );
        assert_eq!(args(&[]).unwrap().overrides.server_filter, None);
    }

    #[test]
    fn persistence_can_be_disabled() {
        assert_eq!(
            args(&["--no-persist"]).unwrap().overrides.persist,
            Some(false)
        );
    }

    #[test]
    fn selects_a_configuration_source() {
        assert_eq!(args(&[]).unwrap().config, ConfigChoice::Default);
        assert_eq!(args(&["--no-config"]).unwrap().config, ConfigChoice::None);
        assert_eq!(
            args(&["--config", r"C:\cfg.toml"]).unwrap().config,
            ConfigChoice::Explicit(PathBuf::from(r"C:\cfg.toml"))
        );
    }

    #[test]
    fn check_config_is_a_mode_and_takes_a_query() {
        assert_eq!(
            args(&["--check-config"]).unwrap().mode,
            Mode::CheckConfig { query: None }
        );
        assert_eq!(
            args(&["--check-config", "--query", "P12345"]).unwrap().mode,
            Mode::CheckConfig {
                query: Some("P12345".into())
            }
        );
    }

    #[test]
    fn check_config_wins_over_bench() {
        assert!(matches!(
            args(&["--bench", "--check-config"]).unwrap().mode,
            Mode::CheckConfig { .. }
        ));
    }

    #[test]
    fn demo_mode_is_recognised() {
        assert!(args(&["--demo"]).unwrap().demo);
    }

    #[test]
    fn an_unknown_argument_is_rejected_with_its_name() {
        let err = args(&["--nonsense"]).unwrap_err();
        assert!(err.0.contains("--nonsense"), "{}", err.0);
    }

    #[test]
    fn a_missing_value_is_rejected_rather_than_silently_ignored() {
        assert!(args(&["--enum"]).unwrap_err().0.contains("--enum"));
        assert!(args(&["--query"]).unwrap_err().0.contains("--query"));
    }

    #[test]
    fn a_bad_value_names_what_was_wrong() {
        let err = args(&["--enum", "banana"]).unwrap_err();
        assert!(err.0.contains("banana"), "{}", err.0);
    }

    #[test]
    fn help_and_version_win_over_later_arguments() {
        assert_eq!(args(&["--help", "--nonsense"]).unwrap().mode, Mode::Help);
    }

    #[test]
    fn the_index_log_path_is_an_override_like_any_other() {
        let args = parse(["--index-log".into(), r"C:\temp\idx.log".into()]).unwrap();
        assert_eq!(
            args.overrides.index_log.as_deref(),
            Some(Path::new(r"C:\temp\idx.log"))
        );
    }

    #[test]
    fn the_index_log_needs_a_path() {
        assert!(parse(["--index-log".into()]).is_err());
    }

    #[test]
    fn the_help_text_documents_every_flag_the_parser_accepts() {
        for flag in [
            "--doctor",
            "--bench",
            "--config",
            "--check-config",
            "--no-config",
            "--query",
            "--allow-write",
            "--enum",
            "--matcher",
            "--server-filter",
            "--no-persist",
            "--demo",
            "--index-log",
            "--viewer",
            "--pdf-viewer",
            "--hotkey",
        ] {
            assert!(HELP.contains(flag), "{flag} is undocumented");
        }
    }

    /// The ENVIRONMENT block in `HELP` is the only documentation of these names.
    #[test]
    fn the_help_text_documents_every_environment_variable_the_loader_reads() {
        for var in [
            "FILES_FS_STRATEGY",
            "FILES_MATCHER",
            "FILES_SERVER_FILTER",
            "FILES_PERSIST",
            "FILES_CACHE_DIR",
            "FILES_INDEX_LOG",
            "FILES_VIEWER",
            "FILES_PDF_VIEWER",
            "FILES_HISTORY",
            "FILES_HOTKEY",
        ] {
            assert!(HELP.contains(var), "{var} is undocumented");
        }

        // Writable settings are enumerable, so check them all rather than
        // relying on the hand-written list above.
        for key in crate::config::write::SettingKey::ALL {
            assert!(HELP.contains(key.env()), "{} is undocumented", key.env());
        }
    }

    #[test]
    fn selects_a_viewer() {
        assert_eq!(
            args(&["--viewer", "sketch"])
                .unwrap()
                .overrides
                .viewer
                .as_deref(),
            Some("sketch")
        );
        let s = args(&["--no-config", "--viewer", "PDF"])
            .unwrap()
            .settings()
            .unwrap();
        assert_eq!(s.viewer, ViewerKind::Pdf);
        assert_eq!(args(&[]).unwrap().overrides.viewer, None);
    }

    /// Validated after the configuration loads; with no config, only the two
    /// built-in viewers exist.
    #[test]
    fn an_unknown_viewer_is_rejected_with_its_name() {
        let errs = args(&["--no-config", "--viewer", "notepad"])
            .unwrap()
            .settings()
            .unwrap_err();
        let msg = crate::config::file::report(&errs);
        assert!(msg.contains("notepad"), "{msg}");
        assert!(msg.contains("[[viewer]]"), "{msg}");
        assert!(args(&["--viewer"]).unwrap_err().0.contains("--viewer"));
    }

    #[test]
    fn names_an_explicit_pdf_viewer() {
        let a = args(&["--pdf-viewer", r"C:	ools\sumatra.exe"]).unwrap();
        assert_eq!(
            a.overrides.pdf_viewer.as_deref(),
            Some(Path::new(r"C:	ools\sumatra.exe"))
        );
        assert!(
            args(&["--pdf-viewer"])
                .unwrap_err()
                .0
                .contains("--pdf-viewer")
        );
    }
}
