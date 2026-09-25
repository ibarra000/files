//! Argument parsing.
//!
//! Hand-rolled rather than pulling in a parser: there are five flags, and
//! keeping the dependency out keeps the binary small and the startup path
//! trivial.

use std::path::PathBuf;

use crate::config::file::ConfigError;
use crate::config::write::SettingKey;
use crate::config::{ConfigChoice, EnumStrategy, MatcherKind, Settings, ViewerKind};
use crate::view::settings::PageId;

/// How a `--bench --walk` run is bounded.
///
/// Kept separate from `walk::WalkOpts` so the CLI can express "unset" and let
/// the defaults come from one place rather than being restated here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkArgs {
    pub concurrency: Option<usize>,
    pub max_depth: Option<u16>,
}

/// What the program was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Run the search panel. The default, and what the program is.
    Gui,
    /// Run the settings window, and nothing else.
    ///
    /// Not a debugging aid. It is how the settings window is opened at all:
    /// the panel starts a copy of itself this way rather than drawing the
    /// form inside its own viewport, which is how Ueli does it and is the
    /// only arrangement in which the window is genuinely independent of the
    /// panel - resizable, in the taskbar, and not fighting a panel that is
    /// always on top.
    ///
    /// It is also a legitimate thing to type. With no panel running the
    /// window opens anyway, reads the configuration file, and writes to it;
    /// what it cannot do is the three things that need a running panel, and
    /// it says so rather than failing when pressed.
    Settings {
        /// Which page to open on. `None` means the one it was last left on,
        /// which is what a toggle should do.
        page: Option<PageId>,
        /// Which settings a command-line flag on the *panel* is holding.
        ///
        /// Without this the window would be a second process with a second
        /// view of what is overridden, and `files --viewer pdf` would give a
        /// settings window that offers to save the viewer, reports success,
        /// and changes nothing at all. The panel passes its own
        /// `Settings::cli_pinned` across. See `config::Pin`.
        pinned: u32,
    },
    /// Fast, read-only capability report.
    Doctor,
    /// Timing comparison across enumeration strategies.
    Bench {
        query: Option<String>,
        allow_write: bool,
        /// Walk the tree recursively and report its shape instead of timing a
        /// single directory. The measurement that decides whether indexing a
        /// whole share is affordable.
        walk: Option<WalkArgs>,
    },
    /// Validate the configuration and print the routing table, without
    /// touching the network.
    CheckConfig {
        query: Option<String>,
    },
    /// Finish an update: wait for the panel to exit, install, start it again.
    ///
    /// Not a thing anybody types. `files.exe` starts a copy of `files-cli.exe`
    /// this way from outside the installation, because an installer cannot
    /// replace the executable that is driving it. See `crate::update::apply`.
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
    pub viewer: Option<ViewerKind>,
    pub pdf_viewer: Option<PathBuf>,
    pub hotkey: Option<crate::hotkey::spec::HotkeySpec>,
}

#[derive(Debug, Clone)]
pub struct Args {
    pub mode: Mode,
    pub config: ConfigChoice,
    pub overrides: Overrides,
    /// Run against an in-memory fake instead of the real drives, so the whole
    /// application can be exercised on a machine without them.
    pub demo: bool,
}

/// Applies an override and records that a flag is holding it.
///
/// One helper rather than a `pin_to_session` line beside each assignment,
/// because the pin is not optional and a line that can be left out will be.
/// Written so that adding the next override means writing the key down.
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
        // The four above are not settings the window can write, so there is
        // nothing for a flag to be holding. The four below are, and every
        // one of them has to say so: a flag outranks the file for this run,
        // so writing the file would report a save the next start ignores.
        //
        // Only `--viewer` used to. The other three set their field and said
        // nothing, so the settings window offered to save a path that
        // `--pdf-viewer` was overruling, reported success, and changed
        // nothing at all - the precise failure `Pin` exists to prevent.
        pinned(&mut s, SettingKey::Viewer, self.overrides.viewer, |s, v| {
            s.viewer = v
        });
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
        // And whatever the *panel's* flags are holding, where this process
        // is the settings window rather than the panel. Merged rather than
        // assigned: a `--viewer` on this line pins the viewer here too, and
        // the two sets are both true.
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
files - job code file search

USAGE:
    files [OPTIONS]
    files --doctor
    files --check-config [--query <CODE>]
    files --bench [--query <CODE>] [--allow-write]
    files --bench --walk [--concurrency <N>] [--max-depth <N>]

MODES:
    (none)              interactive search
    --settings          the settings window on its own. The panel starts one
                        of these; typing it with no panel running opens the
                        window anyway, and every setting still saves.
    --doctor            report drive type, SMB dialect, and capabilities
                        (fast, read-only, safe to run any time)
    --bench             time every enumeration strategy against the real
                        drives and cross-check that they agree
                        (slow: enumerates the share several times)
    --bench --walk      walk every configured share recursively and report
                        how many directories and files it holds, how deep it
                        goes, and how long reading all of it took
                        (read-only, but it reads the entire tree: minutes on
                        a large share. This is the measurement that says
                        whether indexing whole shares is affordable)

OPTIONS:
    --config <PATH>     read this configuration file instead of the default
    --check-config      validate the configuration, print the routing table
                        and exit, without touching the network
    --no-config         ignore any configuration file and use the built-in
                        defaults - the quickest way to rule the file out
    --query <CODE>      job code to use for --bench or --check-config
    --allow-write       let --bench create one temp file, to confirm the
                        directory timestamp actually moves on this server
    --concurrency <N>   directories --bench --walk reads at once (default 8,
                        max 64). Higher finishes sooner and leans harder on
                        the file server; SMB2 credits stop rewarding it well
                        before 64
    --max-depth <N>     how deep --bench --walk descends (default 32)
    --enum <STRATEGY>   handle | findfirst | std        (default: handle)
    --matcher <KIND>    simd | naive                    (default: simd)
    --server-filter <on|off>
                        push the search pattern to the file server
                        (default: off until --bench confirms it is safe)
    --viewer <KIND>     auto | pdf | avwin               (default: auto)
                        auto picks per file: documents assembled, everything
                        else - a drawing included - handed to avwin. pdf
                        gathers every page of the code into one document and
                        opens it with the system's PDF handler; a drawing
                        cannot be one, so it still goes to avwin. avwin opens
                        the single selected file whatever it is. F2 switches
                        while running, but this flag pins it for the run
    --pdf-viewer <PATH> open merged PDFs with this program instead of
                        whatever is registered for .pdf
    --hotkey <CHORD>    the global hotkey that summons the compact quick
                        search window, or \"off\" to claim no key. Modifiers
                        ctrl/alt/shift/win plus a letter, a digit, f1-f24 or
                        space (default: ctrl+shift+space). The Copilot key on
                        newer keyboards sends shift+win+f23, so that value
                        binds the key itself
    --apply-update      finish an update: wait for the running panel to exit,
    --msi <PATH>        install <PATH>, then start it again from --relaunch.
    --wait-pid <PID>    Started by the panel itself from a staging folder,
    --relaunch <PATH>   because an installer cannot replace the executable
                        driving it. Not a thing to type by hand
    --no-persist        do not read or write the on-disk index
    --index-log <PATH>  append one line per index scheduling decision: what
                        woke it, what the directory stamp said, whether it
                        rebuilt and why, and when it will look again
                        (off by default; it answers why did it reindex)
                        how much colour to use. Detection reads COLORTERM and
                        WT_SESSION; NO_COLOR always wins, and FILES_COLOR sets
                        it without a flag. FILES_GLYPHS=ascii replaces the
                        marks for a font that lacks them
    --gui               run the desktop window instead of the terminal
    --demo              run against synthetic data, with no drives at all
    -h, --help          show this
    -V, --version       show the version

CONFIGURATION:
    Mappings live in %APPDATA%\\files\\config.toml, written with sensible
    defaults on first run. Each mapping names a share, says whether it is one
    flat indexed directory or a parent of per-job folders, and carries the
    patterns that route a code to it. Add a share by adding a [[mapping]].

ENVIRONMENT:
    FILES_BASE_PATH, FILES_CUSTPRO_PATH   repoint the 'jobs' / 'custompro'
                                          mappings
    FILES_FS_STRATEGY, FILES_MATCHER, FILES_SERVER_FILTER, FILES_PERSIST,
    FILES_CACHE_DIR, FILES_INDEX_LOG, FILES_VIEWER, FILES_PDF_VIEWER,
    FILES_HISTORY, FILES_HOTKEY, FILES_THEME, FILES_STALE_NOTICES,
    FILES_LIVE_UPDATES, FILES_HIDE_EXTENSIONS, FILES_HIDE_SYSTEM_FILES,
    FILES_PDF_READ_ONLY, FILES_MAX_CONCURRENT_SCANS,
    FILES_UPDATE_FROM, FILES_DEV_MODE, FILES_BACKDROP,
    FILES_RESULT_LAYOUT, FILES_HIDE_ON_BLUR, FILES_HIDE_AFTER_OPENING,
    FILES_HIDE_ON_ESCAPE, FILES_DOCK, FILES_COLUMNS,
    FILES_CHECK_FOR_UPDATES, FILES_UPDATE_GITHUB

    Any of these outranks the configuration file, so a setting changed in the
    settings window applies for the session and is not saved - the window says
    which variable is holding it. Unset the variable to make the change stick.

    FILES_PROBE_INTERVAL, FILES_RESCAN_FLOOR   how often the index checks
                                          whether the share changed, and how
                                          long it may go without a full
                                          rescan, in whole seconds. Lowering
                                          both is how a freshness problem is
                                          reproduced in minutes rather than
                                          hours.

    Every strategy is switchable without a rebuild, so a fast path that
    misbehaves on the real network can be turned off in the field.
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
                let v = value("--viewer")?;
                overrides.viewer = Some(
                    ViewerKind::parse(&v)
                        .ok_or_else(|| ArgError(format!("unknown --viewer value: {v}")))?,
                );
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

    // Above everything, including `--check-config`. This is not a mode
    // somebody chose from a menu of them; it is an instruction from the copy
    // of this program that is about to exit, and anything else on the line
    // would be a mistake rather than a preference.
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

    // Above the text modes and below `--apply-update`, for the same reason
    // as the latter: it is an instruction from the copy of this program that
    // is already running, not a preference somebody expressed alongside
    // others.
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

/// Parses the real process arguments, skipping argv[0].
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

    /// A flag that overrides a setting the window can write has to say so,
    /// or the window offers to save a value the next start will ignore.
    ///
    /// Only `--viewer` did. The other three set their field silently, so
    /// with `--pdf-viewer` given the settings window showed the path as
    /// editable, wrote it to the file, reported a success, and changed
    /// nothing about the running program or the next one.
    ///
    /// `--no-config` so this tests the flags rather than whatever
    /// configuration file the machine running it happens to have.
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

    /// And one nothing overrode is not pinned to the command line, or the
    /// pin would mean nothing.
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

    /// Checking the configuration is the thing you do when something else is
    /// misbehaving, so it takes precedence over the other modes.
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

    /// The ENVIRONMENT block is the only record of these names, and nothing
    /// else fails when one is added to the loader and forgotten here.
    #[test]
    fn the_help_text_documents_every_environment_variable_the_loader_reads() {
        for var in [
            "FILES_BASE_PATH",
            "FILES_CUSTPRO_PATH",
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

        // The list above is hand-written and had already fallen behind -
        // FILES_THEME was read by the loader and named by no one. Everything
        // the settings window can write is enumerable, so that half is asked
        // of the enumeration rather than of somebody's memory.
        for key in crate::config::write::SettingKey::ALL {
            assert!(HELP.contains(key.env()), "{} is undocumented", key.env());
        }
    }

    #[test]
    fn selects_a_viewer() {
        assert_eq!(
            args(&["--viewer", "avwin"]).unwrap().overrides.viewer,
            Some(ViewerKind::Avwin)
        );
        assert_eq!(
            args(&["--viewer", "pdf"]).unwrap().overrides.viewer,
            Some(ViewerKind::Pdf)
        );
        assert_eq!(args(&[]).unwrap().overrides.viewer, None);
    }

    #[test]
    fn an_unknown_viewer_is_rejected_with_its_name() {
        let err = args(&["--viewer", "notepad"]).unwrap_err();
        assert!(err.0.contains("notepad"), "{}", err.0);
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
