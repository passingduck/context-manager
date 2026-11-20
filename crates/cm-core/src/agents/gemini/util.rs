//! Small helpers shared by the Gemini CLI modules.

use std::cmp::Ordering;

use crate::agents::ScanCx;
use crate::model::{AbsPath, Os, RuleRef};
use crate::probe_fs::EntryKind;
use crate::walk;

/// Builds a [`RuleRef`]. Every rule an adapter names goes through here, so a test can find them.
pub(super) fn rule(id: &'static str) -> RuleRef {
    RuleRef::new(id)
}

/// `~` as Gemini CLI resolves it (spec 7): `GEMINI_CLI_HOME`, else the OS home.
pub(super) fn gemini_home(cx: &ScanCx) -> AbsPath {
    cx.env
        .var("GEMINI_CLI_HOME")
        .and_then(|raw| AbsPath::parse(raw).ok())
        .unwrap_or_else(|| cx.env.home.clone())
}

/// JavaScript's default `Array.prototype.sort` on strings: UTF-16 code units (spec 1,
/// "Order inside the project tier").
pub(super) fn cmp_utf16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Gemini CLI compares paths case-insensitively on macOS and Windows (`normalizePath`).
pub(super) fn fold(os: Os, text: &str) -> String {
    match os {
        Os::Macos | Os::Windows => text.to_lowercase(),
        _ => text.to_owned(),
    }
}

/// `isSubpath(root, path)`: equal, or below.
pub(super) fn within(os: Os, root: &AbsPath, path: &AbsPath) -> bool {
    let root = fold(os, root.as_str());
    let path = fold(os, path.as_str());
    path == root || path.starts_with(&format!("{}/", root.trim_end_matches('/')))
}

pub(super) fn kind(cx: &ScanCx, path: &AbsPath) -> Option<EntryKind> {
    cx.fs.stat(path).ok().map(|stat| stat.kind)
}

/// `fs.access(path, R_OK)` for any kind of entry. A directory passes, as in the CLI.
pub(super) fn accessible(cx: &ScanCx, path: &AbsPath) -> bool {
    cx.fs.stat(path).is_ok()
}

pub(super) fn is_dir(cx: &ScanCx, path: &AbsPath) -> bool {
    kind(cx, path) == Some(EntryKind::Dir)
}

pub(super) fn is_file(cx: &ScanCx, path: &AbsPath) -> bool {
    kind(cx, path) == Some(EntryKind::File)
}

pub(super) fn read_lossy(cx: &ScanCx, path: &AbsPath) -> Option<String> {
    let bytes = cx.fs.read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The canonical path, or the path itself when it cannot be resolved.
pub(super) fn real(cx: &ScanCx, path: &AbsPath) -> AbsPath {
    cx.fs.real_path(path).unwrap_or_else(|_| path.clone())
}

/// `path.normalize` for a relative name such as `docs/../AGENTS.md`.
pub(super) fn normalize_name(name: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if matches!(parts.last(), Some(last) if *last != "..") {
                    parts.pop();
                } else {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Files below `dir`, at any depth, whose name ends with `ext`, sorted by path. Symlinks to
/// directories are followed, as the CLI's glob does.
pub(super) fn files_with_ext(cx: &ScanCx, dir: &AbsPath, ext: &str) -> Vec<AbsPath> {
    let mut out: Vec<AbsPath> = walk::list_tree(cx.fs, dir, |_| true)
        .into_iter()
        .filter(|e| e.kind == EntryKind::File && e.name().ends_with(ext))
        .map(|e| e.path)
        .collect();
    out.sort_by(|a, b| cmp_utf16(a.as_str(), b.as_str()));
    out
}

/// JavaScript's `String.prototype.trim`: Unicode white space and the byte order mark.
pub(super) fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// Collapses runs of three or more newlines to two (`promptProvider.ts`).
pub(super) fn collapse_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = 0;
    for ch in text.chars() {
        if ch == '\n' {
            run += 1;
            if run <= 2 {
                out.push(ch);
            }
        } else {
            run = 0;
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_order_puts_uppercase_before_uppercase_g() {
        assert_eq!(cmp_utf16("/r/B/GEMINI.md", "/r/GEMINI.md"), Ordering::Less);
        assert_eq!(cmp_utf16("/r/AGENTS.md", "/r/GEMINI.md"), Ordering::Less);
    }

    #[test]
    fn utf16_order_differs_from_byte_order_above_the_bmp() {
        // U+FF5E is one UTF-16 unit above U+D800; U+1F600 is a surrogate pair that starts at 0xD83D.
        assert_eq!("\u{ff5e}".cmp("\u{1f600}"), Ordering::Less);
        assert_eq!(cmp_utf16("\u{ff5e}", "\u{1f600}"), Ordering::Greater);
    }

    #[test]
    fn normalize_resolves_dots() {
        assert_eq!(normalize_name("docs/../AGENTS.md"), "AGENTS.md");
        assert_eq!(normalize_name("./a//b.md"), "a/b.md");
        assert_eq!(normalize_name("../x.md"), "../x.md");
    }

    #[test]
    fn newline_runs_collapse_to_two() {
        assert_eq!(collapse_newlines("a\n\n\n\nb\n\nc"), "a\n\nb\n\nc");
    }
}
