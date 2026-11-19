//! Search over the text of every file in a snapshot and the raw entry of every hook that does
//! not come from a searched file.

use regex::RegexBuilder;

use crate::model::*;
use crate::scan::Scanned;

pub const MAX_HITS: usize = 500;
/// A preview is cut at this many characters.
const PREVIEW_CHARS: usize = 240;

pub fn search(scanned: &Scanned, req: &SearchReq) -> Result<SearchResult, ApiError> {
    let mut result = SearchResult {
        hits: Vec::new(),
        capped: false,
    };
    if req.query.is_empty() {
        return Ok(result);
    }
    let pattern = if req.regex {
        req.query.clone()
    } else {
        regex::escape(&req.query)
    };
    let matcher = RegexBuilder::new(&pattern)
        .case_insensitive(!req.case_sensitive)
        .size_limit(1 << 20)
        .build()
        .map_err(|_| ApiError::InvalidEdit {
            field: "query".to_owned(),
            why: RuleRef::new("search.invalid_regex"),
        })?;

    let mut searched: Vec<&AbsPath> = Vec::new();
    for path in scanned.snapshot.files.keys() {
        let Some(text) = scanned.file_content(path).and_then(|c| c.text) else {
            continue;
        };
        searched.push(path);
        if !scan_text(&matcher, path, &text, &mut result) {
            return Ok(result);
        }
    }
    for view in &scanned.snapshot.agents {
        for hook in &view.hooks {
            if searched.contains(&&hook.source.file) {
                continue;
            }
            let raw = serde_json::to_string_pretty(&hook.handler.raw).unwrap_or_default();
            if !scan_text(&matcher, &hook.source.file, &raw, &mut result) {
                return Ok(result);
            }
        }
    }
    Ok(result)
}

/// Adds the hits of one text. `false` when the cap stopped the search.
fn scan_text(
    matcher: &regex::Regex,
    file: &AbsPath,
    text: &str,
    result: &mut SearchResult,
) -> bool {
    for (index, line) in text.lines().enumerate() {
        for found in matcher.find_iter(line) {
            if found.range().is_empty() {
                continue;
            }
            if result.hits.len() >= MAX_HITS {
                result.capped = true;
                return false;
            }
            result.hits.push(SearchHit {
                file: file.clone(),
                line: index as u32 + 1,
                column: line[..found.start()].chars().count() as u32 + 1,
                text: preview(line),
            });
        }
    }
    true
}

fn preview(line: &str) -> String {
    let line = line.trim_end();
    match line.char_indices().nth(PREVIEW_CHARS) {
        Some((cut, _)) => line[..cut].to_owned(),
        None => line.to_owned(),
    }
}
