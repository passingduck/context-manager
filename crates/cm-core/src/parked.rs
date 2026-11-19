//! Parked Claude hooks (design section 4, "What the tool owns on disk").
//!
//! Claude Code has no per-hook switch, so switching a hook off removes its handler from the
//! settings file and stores it in `parked/<sha256 of the source file path>.json`. This module
//! holds the record type, the path rule, and the read that scans use. The Claude planner writes a
//! record as an ordinary `Write` operation inside a change set, so the pipeline journals and
//! reverts it like any other file. `cm-core` itself writes nothing.

use serde::{Deserialize, Serialize};

use crate::model::{AbsPath, sha256_hex};
use crate::probe_fs::ProbeFs;

/// One hook that was switched off, with enough to put it back in its old place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParkedHook {
    pub event: String,
    pub matcher: Option<String>,
    /// The handler entry as it was stored, unknown keys included.
    pub handler: serde_json::Value,
    /// Its index among the event's entries when it was removed.
    pub index: usize,
    /// What putting it back needs to restore the exact bytes while nothing else touched the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<Restore>,
}

/// The text that parking took out of a settings file, and where. It applies only while the file
/// is as the removal left it, which `after` pins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Restore {
    /// Byte offset in the file after the removal.
    pub offset: usize,
    /// The text the removal took out.
    pub text: String,
    /// SHA-256 of the file after the removal.
    pub after: String,
}

/// Everything parked from one settings file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParkedFile {
    pub source: AbsPath,
    pub hooks: Vec<ParkedHook>,
}

/// `<parked_dir>/<sha256 of the source path>.json`.
pub fn record_path(parked_dir: &AbsPath, source: &AbsPath) -> AbsPath {
    parked_dir.join(&format!("{}.json", sha256_hex(source.as_str().as_bytes())))
}

pub fn render(file: &ParkedFile) -> String {
    let mut text = serde_json::to_string_pretty(file).expect("a parked record serializes");
    text.push('\n');
    text
}

/// The parked hooks of `source`. A missing record is no hooks. A record that does not parse is
/// also no hooks, so a damaged file cannot stop a scan.
pub fn read(fs: &ProbeFs, parked_dir: &AbsPath, source: &AbsPath) -> Vec<ParkedHook> {
    let Ok(bytes) = fs.read(&record_path(parked_dir, source)) else {
        return Vec::new();
    };
    match serde_json::from_slice::<ParkedFile>(&bytes) {
        Ok(file) if file.source == *source => file.hooks,
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fixture;

    fn hook() -> ParkedHook {
        ParkedHook {
            event: "PreToolUse".into(),
            matcher: Some("Bash".into()),
            handler: serde_json::json!({"type": "command", "command": "echo hi", "extra": 1}),
            index: 2,
            restore: None,
        }
    }

    #[test]
    fn a_record_round_trips_through_the_path_the_planner_and_the_scan_share() {
        let fx = Fixture::new();
        let parked = fx.root().join("data/parked");
        let source = fx.project_path(".claude/settings.json");
        let file = ParkedFile {
            source: source.clone(),
            hooks: vec![hook()],
        };
        let path = record_path(&parked, &source);
        assert!(path.starts_with(&parked));
        fx.write(&path, &render(&file));

        assert_eq!(read(&ProbeFs::new(), &parked, &source), vec![hook()]);
        let other = fx.project_path(".claude/settings.local.json");
        assert!(read(&ProbeFs::new(), &parked, &other).is_empty());
    }

    #[test]
    fn a_damaged_or_foreign_record_reads_as_no_hooks() {
        let fx = Fixture::new();
        let parked = fx.root().join("data/parked");
        let source = fx.project_path(".claude/settings.json");
        fx.write(&record_path(&parked, &source), "{not json");
        assert!(read(&ProbeFs::new(), &parked, &source).is_empty());

        let foreign = ParkedFile {
            source: fx.project_path("elsewhere.json"),
            hooks: vec![hook()],
        };
        fx.write(&record_path(&parked, &source), &render(&foreign));
        assert!(read(&ProbeFs::new(), &parked, &source).is_empty());
    }
}
