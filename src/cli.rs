//! Argument parsing.
//!
//! Hand-rolled rather than pulling in a parser: there are five flags, and
//! keeping the dependency out keeps the binary small and the startup path
//! trivial.

use std::path::PathBuf;

use crate::config::file::ConfigError;
use crate::config::{ConfigChoice, EnumStrategy, MatcherKind, Settings};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Run the interactive search.
    Tui,
    /// Fast, read-only capability report.
    Doctor,
    /// Timing comparison across enumeration strategies.
    Bench {
        query: Option<String>,
        allow_write: bool,
    },
    /// Validate the configuration and print the routing table, without
    /// touching the network.
    CheckConfig {
        query: Option<String>,
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

MODES:
    (none)              interactive search
    --doctor            report drive type, SMB dialect, and capabilities
                        (fast, read-only, safe to run any time)
    --bench             time every enumeration strategy against the real
                        drives and cross-check that they agree
                        (slow: enumerates the share several times)

OPTIONS:
    --config <PATH>     read this configuration file instead of the default
    --check-config      validate the configuration, print the routing table
                        and exit, without touching the network
    --no-config         ignore any configuration file and use the built-in
                        defaults - the quickest way to rule the file out
    --query <CODE>      job code to use for --bench or --check-config
    --allow-write       let --bench create one temp file, to confirm the
                        directory timestamp actually moves on this server
    --enum <STRATEGY>   handle | findfirst | std        (default: handle)
    --matcher <KIND>    simd | naive                    (default: simd)
    --server-filter <on|off>
                        push the search pattern to the file server
                        (default: off until --bench confirms it is safe)
    --no-persist        do not read or write the on-disk index
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
    FILES_CACHE_DIR

    Every strategy is switchable without a rebuild, so a fast path that
    misbehaves on the real network can be turned off in the field.
";

/// Parses arguments over a base of environment-derived settings.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Args, ArgError> {
    let mut mode = Mode::Tui;
    let mut config = ConfigChoice::Default;
    let mut overrides = Overrides::default();
    let mut query = None;
    let mut allow_write = false;
    let mut demo = false;
    let mut check_config = false;

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
                }
            }
            "--check-config" => check_config = true,
            "--config" => config = ConfigChoice::Explicit(PathBuf::from(value("--config")?)),
            "--no-config" => config = ConfigChoice::None,
            "--allow-write" => allow_write = true,
            "--demo" => demo = true,
            "--no-persist" => overrides.persist = Some(false),
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

    // `--check-config` wins over a mode, so `--check-config --query X` shows
    // where X would go rather than benchmarking.
    if check_config {
        mode = Mode::CheckConfig { query };
    } else if let Mode::Bench { .. } = mode {
        mode = Mode::Bench { query, allow_write };
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

    fn args(list: &[&str]) -> Result<Args, ArgError> {
        parse(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_arguments_runs_the_interactive_search() {
        assert_eq!(args(&[]).unwrap().mode, Mode::Tui);
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
                allow_write: false
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
                allow_write: true
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
        ] {
            assert!(HELP.contains(flag), "{flag} is undocumented");
        }
    }
}
