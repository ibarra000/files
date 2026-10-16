//! Upgrades a configuration written by an older version to the current format.
//!
//! An automatic update must never leave the program unable to start, so a
//! stale file is migrated and the user is notified instead of failing.
//!
//! # Version 1 to 2
//!
//! Mapping paths and names are kept. `[[mapping.rules]]`, `case` and `stop`
//! are removed, along with their comments, since every share is now indexed.
//! `kind = "job-folder"` becomes `kind = "tree"`; see [`crate::paths`].
//!
//! # Version 2 to 3
//!
//! `avwin` is no longer a built-in viewer. A file with `viewer = "avwin"` is
//! given an equivalent `[[viewer]]` table so the setting still resolves; other
//! files gain no viewer.
//!
//! Edits use `toml_edit`, as in [`super::write`], so the user's remaining
//! comments and formatting are preserved.

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

use super::file::{CONFIG_VERSION, MAPPING_KEYS, ROOT_KEYS, SETTINGS_KEYS};

/// Why a configuration could not be migrated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    NotToml(String),
    /// The file's `version` is missing or outside the upgradable range.
    CannotUpgradeFrom(Option<i64>),
}

impl Problem {
    pub fn detail(&self) -> String {
        match self {
            Self::NotToml(e) => format!("it is not valid TOML: {e}"),
            Self::CannotUpgradeFrom(Some(v)) => {
                format!("a version {v} configuration cannot be upgraded")
            }
            Self::CannotUpgradeFrom(None) => "it names no version".into(),
        }
    }
}

/// The oldest version that can be upgraded to [`CONFIG_VERSION`].
const OLDEST_UPGRADABLE: i64 = 1;

/// Rewrites an older configuration as a current one.
///
/// Applies one step per version in order. Pure; the caller
/// ([`super::file::load_migrating`]) verifies that the result loads.
pub fn to_current(text: &str) -> Result<String, Problem> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| Problem::NotToml(e.to_string()))?;

    let was = doc.get("version").and_then(Item::as_integer);
    let Some(from) = was.filter(|v| (OLDEST_UPGRADABLE..CONFIG_VERSION).contains(v)) else {
        return Err(Problem::CannotUpgradeFrom(was));
    };

    if from < 2 {
        v1_to_v2(&mut doc);
    }
    if from < 3 {
        v2_to_v3(&mut doc);
    }
    doc["version"] = value(CONFIG_VERSION);

    Ok(doc.to_string())
}

/// Removes routing rules and converts job-folder mappings to trees.
fn v1_to_v2(doc: &mut DocumentMut) {
    if let Some(mappings) = doc
        .get_mut("mapping")
        .and_then(Item::as_array_of_tables_mut)
    {
        for mapping in mappings.iter_mut() {
            if mapping.get("kind").and_then(Item::as_str) == Some("job-folder") {
                mapping["kind"] = value("tree");
            }
            // `rules` is an array of tables; removed explicitly for clarity.
            mapping.remove("rules");
            keep_only(mapping, MAPPING_KEYS);
        }
    }

    if let Some(settings) = doc.get_mut("settings").and_then(Item::as_table_mut) {
        keep_only(settings, SETTINGS_KEYS);
    }

    // Unknown root keys are a startup error, so strip them too.
    let strays: Vec<String> = doc
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !ROOT_KEYS.contains(&key.as_str()))
        .collect();
    for key in strays {
        doc.remove(&key);
    }
}

/// The spellings version 2 accepted for its built-in avwin.
const AVWIN_SPELLINGS: &[&str] = &["avwin", "av", "avwin.exe"];

/// Gives a file that chose avwin an equivalent `[[viewer]]` table.
///
/// The table matches version 2 behaviour: `avwin.exe` on `PATH`, claiming
/// `dwg` from PDF mode.
fn v2_to_v3(doc: &mut DocumentMut) {
    let chose_avwin = doc
        .get("settings")
        .and_then(|s| s.get("viewer"))
        .and_then(Item::as_str)
        .is_some_and(|v| {
            AVWIN_SPELLINGS
                .iter()
                .any(|a| a.eq_ignore_ascii_case(v.trim()))
        });
    if !chose_avwin {
        return;
    }
    // Normalise to the table's name. Rewrite only when needed, because
    // replacing the value drops its trailing comment.
    let spelled = doc["settings"]["viewer"].as_str().map(str::trim);
    if spelled != Some("avwin") {
        doc["settings"]["viewer"] = value("avwin");
    }

    let already = doc
        .get("viewer")
        .and_then(Item::as_array_of_tables)
        .is_some_and(|viewers| {
            viewers.iter().any(|v| {
                v.get("name")
                    .and_then(Item::as_str)
                    .is_some_and(|n| n.trim().eq_ignore_ascii_case("avwin"))
            })
        });
    if already {
        return;
    }

    let mut table = Table::new();
    table["name"] = value("avwin");
    table["program"] = value("avwin.exe");
    let mut extensions = Array::new();
    extensions.push("dwg");
    table["extensions"] = value(extensions);
    table
        .decor_mut()
        .set_prefix("\n# Added during upgrade: avwin is no longer built in.\n");

    match doc.get_mut("viewer").and_then(Item::as_array_of_tables_mut) {
        Some(viewers) => viewers.push(table),
        None => {
            let mut viewers = ArrayOfTables::new();
            viewers.push(table);
            doc["viewer"] = Item::ArrayOfTables(viewers);
        }
    }
}

/// Removes every key not in `allowed`.
///
/// Keys are collected before removal because the table cannot be mutated
/// while iterated.
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

    /// A version 1 file in the shape of the shipped default.
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

    #[test]
    fn a_version_one_configuration_becomes_one_that_loads() {
        let parsed = reload(&migrated());
        assert_eq!(parsed.routes.all().len(), 2);
    }

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

    /// Each of these keys is a startup error in version 2 and later.
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

    /// Unknown keys are removed from the root and `[settings]` as well.
    #[test]
    fn a_stray_key_anywhere_is_swept_up() {
        let text = format!("{V1}\nnonsense = 1\n\n[settings]\npersist = true\nolder = \"x\"\n");
        let migrated = to_current(&text).unwrap();

        assert!(!migrated.contains("nonsense"), "{migrated}");
        assert!(!migrated.contains("older"), "{migrated}");
        assert!(migrated.contains("persist = true"), "{migrated}");
        reload(&migrated);
    }

    /// User comments on mappings are preserved.
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

    /// A version 2 file in the shape of the shipped default, with `viewer` set.
    fn v2(viewer: &str) -> String {
        format!(
            "version = 2\n\n[[mapping]]\nname = \"jobs\"\npath = 'R:\\'\nkind = \"tree\"\nenabled = true\n\n[settings]\n# Default viewer for Enter.\nviewer = \"{viewer}\"   # auto | pdf | avwin\n"
        )
    }

    fn viewers_of(text: &str) -> Vec<(String, String, Vec<String>)> {
        reload(text)
            .viewers
            .all()
            .iter()
            .map(|v| {
                (
                    v.name.to_string(),
                    v.program.to_string(),
                    v.extensions.iter().map(|e| e.to_string()).collect(),
                )
            })
            .collect()
    }

    /// Any accepted avwin spelling yields the same program claiming `dwg`.
    #[test]
    fn a_version_two_file_that_chose_avwin_keeps_it() {
        for spelling in ["avwin", "AV", "AvWin.exe"] {
            let migrated = to_current(&v2(spelling)).unwrap();
            assert_eq!(
                viewers_of(&migrated),
                vec![("avwin".into(), "avwin.exe".into(), vec!["dwg".into()])],
                "{spelling}:\n{migrated}"
            );
            let parsed = reload(&migrated);
            assert_eq!(parsed.settings.viewer.as_deref(), Some("avwin"));
            assert!(
                migrated.contains("# Default viewer for Enter."),
                "{migrated}"
            );
            // The trailing comment survives when the value is not rewritten.
            if spelling == "avwin" {
                assert!(migrated.contains("# auto | pdf | avwin"), "{migrated}");
            }
        }
    }

    #[test]
    fn a_version_two_file_that_did_not_choose_avwin_gains_nothing() {
        for viewer in ["auto", "pdf"] {
            let migrated = to_current(&v2(viewer)).unwrap();
            assert!(viewers_of(&migrated).is_empty(), "{viewer}:\n{migrated}");
            assert!(!migrated.contains("[[viewer]]"), "{migrated}");
        }
        let no_settings =
            "version = 2\n\n[[mapping]]\nname = \"jobs\"\npath = 'R:\\'\nkind = \"tree\"\n";
        assert!(viewers_of(&to_current(no_settings).unwrap()).is_empty());
    }

    /// A second avwin would be a duplicate name and fail to load.
    #[test]
    fn an_avwin_viewer_that_is_already_there_is_not_added_again() {
        let text = format!(
            "{}\n[[viewer]]\nname = \"AVWIN\"\nprogram = 'C:\\tools\\avwin.exe'\n",
            v2("avwin")
        );
        let migrated = to_current(&text).unwrap();
        let viewers = viewers_of(&migrated);
        assert_eq!(viewers.len(), 1, "{migrated}");
        assert_eq!(viewers[0].1, r"C:\tools\avwin.exe");
    }

    #[test]
    fn a_version_one_file_reaches_the_current_version() {
        let text = format!("{V1}\n[settings]\nviewer = \"avwin\"\n");
        let migrated = to_current(&text).unwrap();
        assert!(migrated.contains(&format!("version = {CONFIG_VERSION}")));
        assert_eq!(viewers_of(&migrated).len(), 1, "{migrated}");
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
