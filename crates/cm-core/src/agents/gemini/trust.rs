//! Folder trust (spec 5 "Trust", spec 7 trust variables, `packages/core/src/utils/trust.ts`).

use serde_json::Value;

use super::settings::parse_settings_text;
use super::util::{fold, real};
use crate::agents::ScanCx;
use crate::model::{AbsPath, Os};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TrustSource {
    /// `GEMINI_CLI_TRUST_WORKSPACE` or `GEMINI_RESTRICTED_MODE` decided.
    Env,
    /// `security.folderTrust.enabled` is false: every folder is trusted.
    FeatureOff,
    /// A rule in `trustedFolders.json` matched.
    Rule,
    /// Folder trust is on and no rule matches: the CLI asks, and until answered it behaves as
    /// untrusted (`isTrusted ?? false`).
    NoRule,
}

#[derive(Debug, Clone)]
pub(super) struct TrustInfo {
    pub trusted: bool,
    pub source: TrustSource,
    pub file: AbsPath,
    pub file_exists: bool,
    /// A `trustedFolders.json` that does not parse is fatal.
    pub error: Option<String>,
}

pub(super) fn trusted_folders_path(cx: &ScanCx, gdir: &AbsPath) -> AbsPath {
    match cx.env.var("GEMINI_CLI_TRUSTED_FOLDERS_PATH") {
        Some(raw) => cx.env.cwd.join(raw),
        None => gdir.join("trustedFolders.json"),
    }
}

/// `folder_trust_enabled` comes from the merge of system defaults, user and system settings:
/// the workspace file cannot decide whether it is trusted.
pub(super) fn check(
    cx: &ScanCx,
    gdir: &AbsPath,
    folder_trust_enabled: bool,
    cwd: &AbsPath,
) -> TrustInfo {
    let file = trusted_folders_path(cx, gdir);
    let mut info = TrustInfo {
        trusted: false,
        source: TrustSource::NoRule,
        file,
        file_exists: false,
        error: None,
    };
    let env_is = |name: &str, want: &str| cx.env.var(name) == Some(want);
    if env_is("GEMINI_RESTRICTED_MODE", "true") || env_is("GEMINI_CLI_TRUST_WORKSPACE", "false") {
        info.source = TrustSource::Env;
        return info;
    }
    if env_is("GEMINI_CLI_TRUST_WORKSPACE", "true") {
        info.trusted = true;
        info.source = TrustSource::Env;
        return info;
    }
    if !folder_trust_enabled {
        info.trusted = true;
        info.source = TrustSource::FeatureOff;
        return info;
    }
    let Ok(bytes) = cx.fs.read(&info.file) else {
        return info;
    };
    info.file_exists = true;
    let rules = match parse_settings_text(&String::from_utf8_lossy(&bytes)) {
        Ok(Value::Object(map)) => map,
        Ok(_) => unreachable!("parse_settings_text returns an object"),
        Err(detail) => {
            info.error = Some(detail);
            return info;
        }
    };
    if let Some((raw, _)) = rules.iter().find(|(_, level)| {
        !matches!(
            level.as_str(),
            Some("TRUST_FOLDER" | "TRUST_PARENT" | "DO_NOT_TRUST")
        )
    }) {
        info.error = Some(format!("invalid trust level for {raw}"));
        return info;
    }
    if let Some(verdict) = path_trusted(cx, cx.env.os, &rules, cwd) {
        info.trusted = verdict;
        info.source = TrustSource::Rule;
    }
    info
}

/// `LoadedTrustedFolders.isPathTrusted`: the longest matching rule wins. A `TRUST_PARENT` rule
/// covers the parent of its path.
fn path_trusted(
    cx: &ScanCx,
    os: Os,
    rules: &serde_json::Map<String, Value>,
    cwd: &AbsPath,
) -> Option<bool> {
    let location = fold(os, real(cx, cwd).as_str());
    let mut best: Option<(usize, &str)> = None;
    for (raw, level) in rules {
        let Some(level) = level.as_str() else {
            continue;
        };
        let Ok(rule_path) = AbsPath::parse(raw) else {
            continue;
        };
        let effective = if level == "TRUST_PARENT" {
            rule_path.parent().unwrap_or_else(AbsPath::root)
        } else {
            rule_path.clone()
        };
        let effective = fold(os, real(cx, &effective).as_str());
        let inside = location == effective
            || location.starts_with(&format!("{}/", effective.trim_end_matches('/')));
        let length = fold(os, rule_path.as_str()).len();
        if inside && best.is_none_or(|(len, _)| length > len) {
            best = Some((length, level));
        }
    }
    match best?.1 {
        "DO_NOT_TRUST" => Some(false),
        "TRUST_FOLDER" | "TRUST_PARENT" => Some(true),
        _ => None,
    }
}
