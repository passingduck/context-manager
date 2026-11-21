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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_and_token_cuts_report_their_own_units() {
        let cut = cut_chars("héllo", 2);
        assert_eq!((cut.text.as_str(), cut.kept, cut.total), ("hé", 2, 5));
        let cut = cut_tokens("hello world", 1);
        assert_eq!((cut.text.as_str(), cut.kept, cut.total), ("hello", 1, 2));
    }
}
