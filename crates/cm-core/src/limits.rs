//! The cuts the specs describe. Each returns the kept text and the kept and total sizes.

/// A cut by bytes, characters or tokens. `kept` and `total` are in the cut's own unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cut {
    pub text: String,
    pub kept: u64,
    pub total: u64,
}

impl Cut {
    pub fn is_whole(&self) -> bool {
        self.kept == self.total
    }

    /// Nothing survived: a file after the budget ran out.
    pub fn is_dropped(&self) -> bool {
        self.kept == 0 && self.total > 0
    }
}

/// A cut by whole lines. Sizes are in lines and in bytes, newlines counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineCut {
    pub text: String,
    pub kept_lines: u64,
    pub total_lines: u64,
    pub kept_bytes: u64,
    pub total_bytes: u64,
}

impl LineCut {
    pub fn is_whole(&self) -> bool {
        self.kept_lines == self.total_lines
    }
}

/// Claude `MEMORY.md` (spec 2.4): whole lines while both limits hold. It never cuts inside a
/// line.
pub fn cut_lines(text: &str, max_lines: u64, max_bytes: u64) -> LineCut {
    let mut kept_lines = 0u64;
    let mut kept_bytes = 0u64;
    let mut total_lines = 0u64;
    let mut open = true;
    for line in text.split_inclusive('\n') {
        total_lines += 1;
        let len = line.len() as u64;
        if open && kept_lines < max_lines && kept_bytes + len <= max_bytes {
            kept_lines += 1;
            kept_bytes += len;
        } else {
            open = false;
        }
    }
    LineCut {
        text: text[..kept_bytes as usize].to_owned(),
        kept_lines,
        total_lines,
        kept_bytes,
        total_bytes: text.len() as u64,
    }
}

/// Codex (spec 1.3): a byte cut that may split a character, decoded lossily, so a split
/// character becomes U+FFFD.
pub fn cut_bytes_lossy(bytes: &[u8], max: u64) -> Cut {
    let kept = (bytes.len() as u64).min(max);
    Cut {
        text: String::from_utf8_lossy(&bytes[..kept as usize]).into_owned(),
        kept,
        total: bytes.len() as u64,
    }
}

/// A cut at a character count.
pub fn cut_chars(text: &str, max: u64) -> Cut {
    let total = text.chars().count() as u64;
    let end = text
        .char_indices()
        .nth(max as usize)
        .map_or(text.len(), |(at, _)| at);
    Cut {
        text: text[..end].to_owned(),
        kept: total.min(max),
        total,
    }
}

/// A cut at an `o200k_base` token count.
pub fn cut_tokens(text: &str, max: u32) -> Cut {
    let (kept_text, total) = crate::tokens::take(text, max);
    Cut {
        text: kept_text,
        kept: u64::from(total.min(max)),
        total: u64::from(total),
    }
}

/// Spends one byte budget across `files` in order (the Codex project chain, spec 1.3). The file
/// that crosses the budget is cut at the byte. Every later file gets nothing.
pub fn fold_byte_budget(budget: u64, files: &[&[u8]]) -> Vec<Cut> {
    let mut remaining = budget;
    files
        .iter()
        .map(|bytes| {
            let cut = cut_bytes_lossy(bytes, remaining);
            remaining -= cut.kept;
            cut
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_memory_keeps_83_of_150_lines_of_298_bytes() {
        // Spec 2.4, RUN:bytes: 83 x 298 = 24,734 bytes kept under the 25,000-byte limit.
        let line = format!("{}\n", "x".repeat(297));
        assert_eq!(line.len(), 298);
        let text = line.repeat(150);
        let cut = cut_lines(&text, 200, 25_000);
        assert_eq!(cut.kept_lines, 83);
        assert_eq!(cut.total_lines, 150);
        assert_eq!(cut.kept_bytes, 24_734);
        assert_eq!(cut.text.len(), 24_734);
    }

    #[test]
    fn the_line_limit_binds_before_the_byte_limit() {
        let text = "a\n".repeat(250);
        let cut = cut_lines(&text, 200, 25_000);
        assert_eq!((cut.kept_lines, cut.total_lines), (200, 250));
        assert!(cut_lines("a\nb", 200, 25_000).is_whole());
    }

    #[test]
    fn a_codex_budget_cuts_the_second_file_inside_a_character_and_drops_the_third() {
        let first = b"abcd".as_slice();
        // 'é' is 0xC3 0xA9. The budget leaves 3 bytes: "xy" and the first byte of 'é'.
        let second = "xyé-tail".as_bytes();
        let third = b"never".as_slice();
        let cuts = fold_byte_budget(7, &[first, second, third]);
        assert_eq!(cuts[0].text, "abcd");
        assert_eq!(cuts[1].text, "xy\u{FFFD}");
        assert_eq!((cuts[1].kept, cuts[1].total), (3, 9));
        assert!(cuts[2].is_dropped());
        assert_eq!(cuts[2].text, "");
    }

    #[test]
    fn a_zero_budget_drops_every_file() {
        let cuts = fold_byte_budget(0, &[b"a".as_slice(), b"b".as_slice()]);
        assert!(cuts.iter().all(Cut::is_dropped));
    }

    #[test]
    fn character_and_token_cuts_report_their_own_units() {
        let cut = cut_chars("héllo", 2);
        assert_eq!((cut.text.as_str(), cut.kept, cut.total), ("hé", 2, 5));
        let cut = cut_tokens("hello world", 1);
        assert_eq!((cut.text.as_str(), cut.kept, cut.total), ("hello", 1, 2));
    }
}
