//! Bringing a configuration written by an older version forward.
//!
//! # Why this exists at all, when the file already says what to do
//!
//! A version 1 file is a hard startup failure with a message explaining what
//! to edit, and for one person at one desk that is a reasonable answer: they
//! made the file, they can fix it.
//!
//! It stops being reasonable the moment a new version is rolled out to a
//! fleet. Everybody whose file is stale would find the program refusing to
//! start, all at once, because of an upgrade they did not ask for and cannot
//! undo - which is the one failure an automatic update must never cause. So
//! the file is brought forward instead, and the person is told it happened.
//!
//! # What is kept, and what is not
//!
//! The paths. They are the part somebody typed, the part that differs between
//! one installation and the next, and the part no default could guess. A
//! migration that threw them away and wrote the shipped file would be a
//! migration that silently repointed every drive.
//!
//! Everything else in a version 1 file describes work this program no longer
//! does. `[[mapping.rules]]` was a list of regular expressions that turned a
//! code into the one folder it was expected to live in, with `case` and `stop`
//! to control them; every share is indexed now, so there is nothing left for a
//! pattern to decide. Those go, and the comments explaining them go with them,
//! because a comment about a deleted mechanism is worse than no comment.
//!
//! `kind = "job-folder"` becomes `kind = "tree"`. That is not a rename: it is
//! what replaced it. The old kind deduced a folder from the code and looked
//! only there, and failed silently for every folder the rule did not predict.
//! A tree is walked whole and searched, which answers the same question by
//! looking rather than by guessing. See [`crate::paths`].
//!
//! # Format-preserving, like every other write to this file
//!
//! `toml_edit` again, and for the reason [`super::write`] gives: the file is
//! meant to be hand-edited, and re-serialising it from a parsed model would
//! hand it back with every remaining comment gone. What is removed here is
//! removed deliberately; nothing else is touched.

use toml_edit::{DocumentMut, Item, Table, value};

use super::file::{CONFIG_VERSION, MAPPING_KEYS, ROOT_KEYS, SETTINGS_KEYS};

/// Why a configuration could not be brought forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    NotToml(String),
    /// Not a version this knows how to upgrade *from*.
    ///
    /// Carries what it said, because "version 4" from a future build is a
    /// very different thing to explain than "no version at all".
    CannotUpgradeFrom(Option<i64>),
}

impl Problem {
    pub fn detail(&self) -> String {
        match self {
            Self::NotToml(e) => format!("it is not valid TOML: {e}"),
            Self::CannotUpgradeFrom(Some(v)) => {
                format!("there is no way to bring a version {v} configuration forward")
            }
            Self::CannotUpgradeFrom(None) => "it names no version".into(),
        }
    }
}

/// The highest version this can upgrade from.
///
/// One, because one is all there has ever been. Named so that the next bump
/// has somewhere obvious to go, and so the test below can say what it means.
const OLDEST_UPGRADABLE: i64 = 1;

/// Rewrites a version 1 configuration as a version 2 one.
///
/// Pure, like [`super::write::apply`], so every shape a real file can be in is
/// testable without one. Proving the result loads is the caller's job - see
/// [`super::file::load_migrating`] - for the same reason the writer proves it
/// there: what matters is not that this produced valid TOML but that the next
/// start will accept it.
pub fn to_current(text: &str) -> Result<String, Problem> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| Problem::NotToml(e.to_string()))?;

    let was = doc.get("version").and_then(Item::as_integer);
    if was != Some(OLDEST_UPGRADABLE) {
        return Err(Problem::CannotUpgradeFrom(was));
    }
    doc["version"] = value(CONFIG_VERSION);

    if let Some(mappings) = doc
        .get_mut("mapping")
        .and_then(Item::as_array_of_tables_mut)
    {
        for mapping in mappings.iter_mut() {
            // Before the keys are filtered, because `kind` survives the filter
            // and has to be the new spelling by the time it does.
            if mapping.get("kind").and_then(Item::as_str) == Some("job-folder") {
                mapping["kind"] = value("tree");
            }
            // Named as well as filtered. `rules` is an array of tables rather
            // than a key, and it is the one thing here whose removal is the
            // point rather than a consequence.
            mapping.remove("rules");
            keep_only(mapping, MAPPING_KEYS);
        }
    }

    if let Some(settings) = doc.get_mut("settings").and_then(Item::as_table_mut) {
        keep_only(settings, SETTINGS_KEYS);
    }

    // The root, last. A version 1 file should hold nothing else, but an
    // unknown key here is a hard startup error, and leaving one behind would
    // mean the migration produced a file that still would not load.
    let strays: Vec<String> = doc
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !ROOT_KEYS.contains(&key.as_str()))
        .collect();
    for key in strays {
        doc.remove(&key);
    }

    Ok(doc.to_string())
}

/// Drops every key the current format does not accept.
///
/// Collected first and removed after, because a table cannot be iterated and
/// mutated at once - and because removing by name is what keeps this readable
/// against `toml_edit`, where a key is not simply a string.
fn keep_only(table: &mut Table, allowed: &[&str]) {
    let strays: Vec<String> = table
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !allowed.contains(&key.as_str()))
        .collect();
    for key in strays {
        table.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::file;
    use std::path::Path;

    /// A version 1 file, in the shape the shipped one actually had.
    const V1: &str = r#"
version = 1

[[mapping]]
name    = "custompro"
path    = 'V:\Documents\custpro'
kind    = "flat"
enabled = true
stop    = true

  [[mapping.rules]]
  pattern = '^PP'

[[mapping]]
name    = "jobs"
path    = 'R:\'
kind    = "job-folder"
enabled = true
case    = "lower"

  # Four fields, single-LETTER second field:  11-D-0704-A2 -> 11d
  [[mapping.rules]]
  pattern = '^([A-Z0-9]+)-([A-Z])-([A-Z0-9]+)-([A-Z0-9]+)$'
  folder  = '${1}${2}'
"#;

    fn migrated() -> String {
        to_current(V1).expect("the shipped version 1 file must migrate")
    }

    fn reload(text: &str) -> file::ParsedConfig {
        match file::parse(
            text,
            Path::new("cfg.toml"),
            crate::paths::ConfigSource::BuiltIn,
        ) {
            Ok(c) => c,
            Err(errs) => panic!("a migrated file must load:\n{}", file::report(&errs)),
        }
    }

    /// The whole point: what comes out is a file this build will start on.
    #[test]
    fn a_version_one_configuration_becomes_one_that_loads() {
        let parsed = reload(&migrated());
        assert_eq!(parsed.routes.all().len(), 2);
    }

    /// The part somebody typed, and the part no default could guess.
    #[test]
    fn every_configured_path_survives_the_migration() {
        let parsed = reload(&migrated());
        let paths: Vec<String> = parsed
            .routes
            .all()
            .iter()
            .map(|m| m.path.display().to_string())
            .collect();
        assert!(paths.iter().any(|p| p.contains("custpro")), "{paths:?}");
        assert!(paths.iter().any(|p| p.starts_with("R:")), "{paths:?}");
    }

    #[test]
    fn the_names_survive_too() {
        let parsed = reload(&migrated());
        let names = parsed.routes.names();
        assert!(names.contains(&"custompro"), "{names:?}");
        assert!(names.contains(&"jobs"), "{names:?}");
    }

    /// Not a rename. A job folder deduced one directory from the code and
    /// looked only there; a tree is walked whole and searched.
    #[test]
    fn a_job_folder_becomes_a_tree() {
        let parsed = reload(&migrated());
        let jobs = parsed
            .routes
            .all()
            .iter()
            .find(|m| m.name.as_ref() == "jobs")
            .expect("jobs must survive");
        assert_eq!(jobs.kind, crate::paths::MappingKind::Tree);
    }

    #[test]
    fn a_flat_mapping_stays_flat() {
        let parsed = reload(&migrated());
        let custpro = parsed
            .routes
            .all()
            .iter()
            .find(|m| m.name.as_ref() == "custompro")
            .expect("custompro must survive");
        assert_eq!(custpro.kind, crate::paths::MappingKind::Flat);
    }

    /// Every one of these is a hard startup error in version 2, so leaving
    /// any behind would produce a file that still would not load.
    #[test]
    fn nothing_the_rule_engine_needed_is_left_behind() {
        let text = migrated();
        for gone in ["rules", "pattern", "folder", "case", "stop", "job-folder"] {
            assert!(!text.contains(gone), "{gone:?} survived:\n{text}");
        }
    }

    #[test]
    fn the_version_is_the_one_this_build_understands() {
        assert!(migrated().contains(&format!("version = {CONFIG_VERSION}")));
    }

    /// A key version 2 does not know is a hard startup error wherever it sits,
    /// so the migration has to sweep the root and the settings table too.
    #[test]
    fn a_stray_key_anywhere_is_swept_up() {
        let text = format!("{V1}\nnonsense = 1\n\n[settings]\npersist = true\nolder = \"x\"\n");
        let migrated = to_current(&text).unwrap();

        assert!(!migrated.contains("nonsense"), "{migrated}");
        assert!(!migrated.contains("older"), "{migrated}");
        assert!(migrated.contains("persist = true"), "{migrated}");
        reload(&migrated);
    }

    /// The comments on the mappings are the user's own notes about their own
    /// drives, and they outlive the rule engine.
    #[test]
    fn a_comment_that_is_not_about_the_rules_survives() {
        let text = V1.replace(
            "[[mapping]]\nname    = \"jobs\"",
            "# The drawings, on the old server.\n[[mapping]]\nname    = \"jobs\"",
        );
        let migrated = to_current(&text).unwrap();
        assert!(
            migrated.contains("# The drawings, on the old server."),
            "{migrated}"
        );
    }

    #[test]
    fn a_file_that_is_already_current_is_not_migrated_again() {
        let text = format!("version = {CONFIG_VERSION}\n");
        assert_eq!(
            to_current(&text),
            Err(Problem::CannotUpgradeFrom(Some(CONFIG_VERSION)))
        );
    }

    #[test]
    fn a_version_from_the_future_is_refused_rather_than_mangled() {
        assert_eq!(
            to_current("version = 99\n"),
            Err(Problem::CannotUpgradeFrom(Some(99)))
        );
        assert_eq!(to_current(""), Err(Problem::CannotUpgradeFrom(None)));
    }

    #[test]
    fn something_that_is_not_toml_is_refused() {
        assert!(matches!(to_current("{{{"), Err(Problem::NotToml(_))));
    }

    #[test]
    fn every_problem_explains_itself() {
        for problem in [
            Problem::NotToml("x".into()),
            Problem::CannotUpgradeFrom(Some(9)),
            Problem::CannotUpgradeFrom(None),
        ] {
            assert!(!problem.detail().is_empty());
        }
    }
}
