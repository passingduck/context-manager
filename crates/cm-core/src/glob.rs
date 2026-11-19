//! Two matchers.
//!
//! [`RuleGlobs`] is the `paths` list of a Claude rule (spec 1.4): gitignore-style matching of a
//! path relative to a base. [`ExcludeGlobs`] is `claudeMdExcludes` (spec 1.7): picomatch-style
//! globs against absolute paths.

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::model::AbsPath;

/// Brace expansion stops at this many patterns per rule (spec 1.4).
pub const BRACE_PATTERN_BUDGET: usize = 1_000;
/// Brace expansion stops at this many bytes of expanded patterns per rule (spec 1.4).
pub const BRACE_BYTE_BUDGET: usize = 4 * 1024 * 1024;

/// Splits a comma-separated `paths` string. A comma inside `{}` does not split (spec 1.4).
pub fn split_paths_string(raw: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for ch in raw.chars() {
        match ch {
            '{' => {
                depth += 1;
                current.push(ch);
            }
            '}' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if depth == 0 => parts.push(std::mem::take(&mut current)),
            _ => current.push(ch),
        }
    }
    parts.push(current);
    parts
        .into_iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Expands `{a,b}` groups, nested groups included. `None` when the result passes the budget
/// of 1,000 patterns or 4 MiB.
pub fn expand_braces(pattern: &str) -> Option<Vec<String>> {
    let mut out = vec![pattern.to_owned()];
    loop {
        let mut next = Vec::with_capacity(out.len());
        let mut changed = false;
        for item in &out {
            match expand_once(item) {
                Some(parts) => {
                    changed = true;
                    next.extend(parts);
                }
                None => next.push(item.clone()),
            }
        }
        if !changed {
            return Some(out);
        }
        let bytes: usize = next.iter().map(String::len).sum();
        if next.len() > BRACE_PATTERN_BUDGET || bytes > BRACE_BYTE_BUDGET {
            return None;
        }
        out = next;
    }
}

/// Expands the first top-level group of `s` that has a comma, or returns `None`.
fn expand_once(s: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == '{' {
            let mut depth = 0;
            let mut commas = Vec::new();
            let mut j = i;
            while j < chars.len() {
                match chars[j] {
                    '\\' => j += 1,
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    ',' if depth == 1 => commas.push(j),
                    _ => {}
                }
                j += 1;
            }
            if j < chars.len() && !commas.is_empty() {
                let head: String = chars[..i].iter().collect();
                let tail: String = chars[j + 1..].iter().collect();
                let mut bounds = vec![i];
                bounds.extend(commas);
                bounds.push(j);
                return Some(
                    bounds
                        .windows(2)
                        .map(|w| {
                            let mid: String = chars[w[0] + 1..w[1]].iter().collect();
                            format!("{head}{mid}{tail}")
                        })
                        .collect(),
                );
            }
        }
        i += 1;
    }
    None
}

enum Rule {
    Unconditional,
    Patterns(Vec<String>),
}

/// The `paths` of a Claude rule.
pub struct RuleGlobs {
    rule: Rule,
}

impl RuleGlobs {
    /// Applies the quirks of spec 1.4. Braces expand, a pattern over budget stays literal
    /// and matches nothing, a trailing `/**` is stripped, and a list that is empty or all `**`
    /// is unconditional.
    pub fn new(patterns: &[String]) -> RuleGlobs {
        let mut expanded = Vec::new();
        for pattern in patterns {
            match expand_braces(pattern) {
                Some(list) => expanded.extend(list),
                // Over budget: used literally, so the braces lose their meaning.
                None => expanded.push(pattern.replace('{', "\\{").replace('}', "\\}")),
            }
        }
        let stripped: Vec<String> = expanded
            .into_iter()
            .map(|p| p.strip_suffix("/**").map_or(p.clone(), str::to_owned))
            .collect();
        if stripped.iter().all(|p| p == "**") {
            return RuleGlobs {
                rule: Rule::Unconditional,
            };
        }
        RuleGlobs {
            rule: Rule::Patterns(stripped),
        }
    }

    /// A rule with no `paths`.
    pub fn unconditional() -> RuleGlobs {
        RuleGlobs {
            rule: Rule::Unconditional,
        }
    }

    pub fn is_unconditional(&self) -> bool {
        matches!(self.rule, Rule::Unconditional)
    }

    /// Whether `target` matches, taken relative to `base`. A target outside `base` never
    /// matches. A pattern that is not valid gitignore syntax matches nothing.
    pub fn matches(&self, base: &AbsPath, target: &AbsPath) -> bool {
        let Rule::Patterns(patterns) = &self.rule else {
            return true;
        };
        let Some(rel) = relative_to(base, target) else {
            return false;
        };
        let mut builder = GitignoreBuilder::new(base.as_path());
        for pattern in patterns {
            // A bad bracket expression matches nothing.
            if globset::Glob::new(pattern.trim_start_matches('!')).is_ok() {
                let _ = builder.add_line(None, pattern);
            }
        }
        let Ok(set): Result<Gitignore, _> = builder.build() else {
            return false;
        };
        set.matched_path_or_any_parents(&rel, false).is_ignore()
    }
}

/// `target` relative to `base`, or `None` when it is the base itself or outside it.
fn relative_to(base: &AbsPath, target: &AbsPath) -> Option<String> {
    let rest = target.as_str().strip_prefix(base.as_str())?;
    let rest = if base.as_str() == "/" {
        rest
    } else {
        rest.strip_prefix('/')?
    };
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// `claudeMdExcludes` (spec 1.7): globs matched against absolute paths, dotfiles included.
pub struct ExcludeGlobs {
    set: globset::GlobSet,
}

impl ExcludeGlobs {
    /// A pattern that does not compile matches nothing.
    pub fn new(patterns: &[String]) -> ExcludeGlobs {
        let mut builder = globset::GlobSetBuilder::new();
        for pattern in patterns {
            if let Ok(glob) = globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
            {
                builder.add(glob);
            }
        }
        ExcludeGlobs {
            set: builder
                .build()
                .unwrap_or_else(|_| globset::GlobSet::empty()),
        }
    }

    /// Whether any of `paths` matches. For a symlinked rule pass the link path and its target:
    /// a match on either excludes it.
    pub fn excludes(&self, paths: &[&AbsPath]) -> bool {
        paths.iter().any(|p| self.set.is_match(p.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(p: &str) -> AbsPath {
        AbsPath::parse(p).unwrap()
    }

    fn rule(patterns: &[&str]) -> RuleGlobs {
        RuleGlobs::new(&patterns.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_pattern_without_a_slash_matches_at_any_depth() {
        let r = rule(&["*.txt"]);
        assert!(r.matches(&abs("/p/app"), &abs("/p/app/nested/file.txt")));
        assert!(!r.matches(&abs("/p/app"), &abs("/p/app/nested/file.md")));
    }

    #[test]
    fn a_leading_slash_anchors_to_the_base() {
        let r = rule(&["/src/*.rs"]);
        assert!(r.matches(&abs("/p"), &abs("/p/src/a.rs")));
        assert!(!r.matches(&abs("/p"), &abs("/p/x/src/a.rs")));
    }

    #[test]
    fn a_trailing_double_star_is_stripped_so_the_directory_matches_its_files() {
        let r = rule(&["repo/lib/**"]);
        assert!(r.matches(&abs("/p"), &abs("/p/repo/lib/deep/a.ts")));
        assert!(!r.matches(&abs("/p"), &abs("/p/repo/other/a.ts")));
    }

    #[test]
    fn an_empty_list_or_all_double_stars_is_unconditional() {
        assert!(rule(&[]).is_unconditional());
        assert!(rule(&["**", "**"]).is_unconditional());
        assert!(!rule(&["**", "*.md"]).is_unconditional());
        assert!(rule(&["**"]).matches(&abs("/p"), &abs("/elsewhere/x")));
    }

    #[test]
    fn a_target_outside_the_base_never_matches() {
        let r = rule(&["*.md"]);
        assert!(!r.matches(&abs("/p/app"), &abs("/p/other/a.md")));
        assert!(!r.matches(&abs("/p/app"), &abs("/p/app")));
    }

    #[test]
    fn a_bad_bracket_matches_nothing() {
        let r = rule(&["[unclosed"]);
        assert!(!r.is_unconditional());
        assert!(!r.matches(&abs("/p"), &abs("/p/[unclosed")));
        assert!(!r.matches(&abs("/p"), &abs("/p/a.md")));
    }

    #[test]
    fn braces_expand_and_a_comma_inside_them_does_not_split() {
        assert_eq!(
            split_paths_string("src/**/*.{ts,tsx}, docs/*.md"),
            ["src/**/*.{ts,tsx}", "docs/*.md"]
        );
        let r = rule(&["src/**/*.{ts,tsx}"]);
        assert!(r.matches(&abs("/p"), &abs("/p/src/a/b.tsx")));
        assert!(!r.matches(&abs("/p"), &abs("/p/src/a/b.js")));
        assert_eq!(expand_braces("a{b,c{d,e}}").unwrap(), ["ab", "acd", "ace"]);
    }

    #[test]
    fn brace_expansion_has_a_budget() {
        // 10 groups of two options expand to 1,024 patterns, over the 1,000 budget.
        let big = "{a,b}".repeat(10);
        assert!(expand_braces(&big).is_none());
        let r = rule(&[&big]);
        assert!(!r.is_unconditional());
        assert!(!r.matches(&abs("/p"), &abs("/p/aaaaaaaaaa")));
        assert_eq!(expand_braces(&"{a,b}".repeat(9)).unwrap().len(), 512);
    }

    #[test]
    fn exclude_patterns_match_absolute_paths_with_dotfiles() {
        let ex = ExcludeGlobs::new(&[
            "**/top/CLAUDE.md".to_owned(),
            "**/.claude/rules/sub/**".to_owned(),
        ]);
        assert!(ex.excludes(&[&abs("/w/top/CLAUDE.md")]));
        assert!(ex.excludes(&[&abs("/w/p/.claude/rules/sub/a.md")]));
        assert!(!ex.excludes(&[&abs("/w/p/.claude/rules/a.md")]));
        // The link or its target may match.
        assert!(ex.excludes(&[&abs("/w/link.md"), &abs("/w/top/CLAUDE.md")]));
    }
}
