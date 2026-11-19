//! The system prompt override (spec 6, `GEMINI_SYSTEM_MD`) and the `.env` file that can set it
//! (spec 2, ".env file").

use super::util::{is_file, read_lossy};
use super::world::World;
use crate::model::AbsPath;

/// The file `GEMINI_SYSTEM_MD` selects.
pub(super) struct SystemMd {
    pub path: AbsPath,
    /// `1` or `true`, as opposed to a path.
    pub from_switch: bool,
}

/// `resolvePathFromEnv`: `0`, `false` or unset keep the built-in prompt, `1` or `true` mean
/// `<cwd>/.gemini/system.md`, and anything else is a path.
pub(super) fn system_md(w: &World) -> Option<SystemMd> {
    let raw =
        w.cx.env
            .var("GEMINI_SYSTEM_MD")
            .map(str::to_owned)
            .or_else(|| dotenv_value(w, "GEMINI_SYSTEM_MD"))?;
    let trimmed = raw.trim();
    match trimmed.to_lowercase().as_str() {
        "" | "0" | "false" => None,
        "1" | "true" => Some(SystemMd {
            path: w.cwd.join(".gemini/system.md"),
            from_switch: true,
        }),
        _ => {
            let path = if trimmed == "~" {
                w.home.clone()
            } else if let Some(rest) = trimmed.strip_prefix("~/") {
                w.home.join(rest)
            } else {
                w.cwd.join(trimmed)
            };
            Some(SystemMd {
                path,
                from_switch: false,
            })
        }
    }
}

/// `GEMINI_WRITE_SYSTEM_MD` on: the CLI overwrites the target file with the composed prompt.
pub(super) fn write_target(w: &World) -> Option<AbsPath> {
    let raw = w.cx.env.var("GEMINI_WRITE_SYSTEM_MD")?.trim().to_owned();
    match raw.to_lowercase().as_str() {
        "" | "0" | "false" => None,
        "1" | "true" => Some(w.cwd.join(".gemini/system.md")),
        _ => Some(if let Some(rest) = raw.strip_prefix("~/") {
            w.home.join(rest)
        } else {
            w.cwd.join(&raw)
        }),
    }
}

/// `findEnvFile`: from the working directory up, `<d>/.gemini/.env` (trusted only), then
/// `<d>/.env`; at the root, `~/.gemini/.env` (trusted only), then `~/.env`. The first file
/// wins. An untrusted folder loads only four credential variables, so `GEMINI_SYSTEM_MD` is
/// never read from it.
fn dotenv_value(w: &World, key: &str) -> Option<String> {
    if !w.trusted() {
        return None;
    }
    let mut candidates: Vec<AbsPath> = Vec::new();
    let mut dir = Some(w.cwd.clone());
    while let Some(current) = dir {
        candidates.push(current.join(".gemini/.env"));
        candidates.push(current.join(".env"));
        dir = current.parent();
    }
    candidates.push(w.gdir.join(".env"));
    candidates.push(w.home.join(".env"));
    let file = candidates.into_iter().find(|p| is_file(w.cx, p))?;
    read_lossy(w.cx, &file)?.lines().find_map(|line| {
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        let (name, value) = line.split_once('=')?;
        if name.trim() != key {
            return None;
        }
        let value = value.trim();
        let unquoted = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        Some(unquoted.to_owned())
    })
}
