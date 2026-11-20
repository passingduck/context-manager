//! What the tool must never write (spec 8) and what it must not write broken.

use super::settings::parse_settings_text;
use super::util::rule;
use crate::model::{AbsPath, ApiError, ReadOnlyReason};

/// A path the CLI owns: approving through it would bypass a security prompt, or the CLI would
/// overwrite the edit (spec 8).
pub(super) fn is_cli_state(path: &AbsPath) -> bool {
    let p = path.as_str();
    let name = path.file_name().unwrap_or("");
    let under_dot_gemini = p.contains("/.gemini/");
    let state_file = under_dot_gemini
        && matches!(
            name,
            "trusted_hooks.json" | "trustedFolders.json" | "projects.json" | ".project_root"
        );
    let acknowledgments = p.contains("/.gemini/acknowledgments/") && name == "agents.json";
    let memory_service = p.contains("/.gemini/tmp/")
        && p.contains("/memory/")
        && (p.contains("/memory/.inbox/")
            || name == ".extraction-state.json"
            || name == ".extraction.lock");
    state_file || acknowledgments || memory_service || is_system_file(p)
}

fn is_system_file(p: &str) -> bool {
    p.starts_with("/etc/gemini-cli/") || p.starts_with("/Library/Application Support/GeminiCli/")
}

fn is_settings_file(path: &AbsPath) -> bool {
    path.file_name() == Some("settings.json") && path.as_str().contains("/.gemini/")
}

pub(super) fn guard(
    path: &AbsPath,
    before: Option<&str>,
    after: Option<&str>,
) -> Result<(), ApiError> {
    if is_cli_state(path) {
        return Err(ApiError::ReadOnly {
            why: ReadOnlyReason::Rule {
                rule: rule("gemini.state_file"),
            },
        });
    }
    // A settings file that does not parse makes the next launch fatal (spec 2, "Comments").
    if is_settings_file(path)
        && let Some(after) = after
        && parse_settings_text(after).is_err()
        && before.is_none_or(|b| parse_settings_text(b).is_ok())
    {
        return Err(ApiError::InvalidEdit {
            field: "text".to_owned(),
            why: rule("gemini.settings_parse"),
        });
    }
    Ok(())
}
