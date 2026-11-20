//! `o200k_base` token counts. The encoder loads once per process. Counts are cached by
//! [`Revision`], so a file that several agents read is tokenized once. A text over the analysis
//! bound, [`crate::md::MAX_ANALYZED_BYTES`], is counted from its first part.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tiktoken_rs::{CoreBPE, o200k_base_singleton};

use crate::md::analyzed;
use crate::model::Revision;

fn encoder() -> &'static CoreBPE {
    o200k_base_singleton()
}

static CACHE: OnceLock<Mutex<HashMap<Revision, u32>>> = OnceLock::new();

/// Token counts of analyzed parts of at least [`MEMO_MIN_BYTES`], by their revision.
static HEADS: OnceLock<Mutex<HashMap<Revision, usize>>> = OnceLock::new();

/// A text this long is counted once per content: hashing it costs a twentieth of counting it.
/// An adapter counts one imported file once per import and again in each block that holds it.
const MEMO_MIN_BYTES: usize = 64 * 1024;

/// Tokens in `text`. Special-token strings in the text count as plain text. A text over the
/// analysis bound gets the count of its analyzed part, scaled by its length.
pub fn count(text: &str) -> u32 {
    let head = analyzed(text);
    scale(head_tokens(head), head.len(), text.len())
}

fn head_tokens(head: &str) -> usize {
    if head.len() < MEMO_MIN_BYTES {
        return encoder().encode_ordinary(head).len();
    }
    let memo = HEADS.get_or_init(Default::default);
    let key = Revision::of(head.as_bytes());
    if let Some(hit) = memo.lock().expect("token memo").get(&key) {
        return *hit;
    }
    let n = encoder().encode_ordinary(head).len();
    memo.lock().expect("token memo").insert(key, n);
    n
}

/// `tokens` in `counted` bytes, scaled to `total` bytes.
fn scale(tokens: usize, counted: usize, total: usize) -> u32 {
    let tokens = if counted == total {
        tokens as u128
    } else {
        (tokens as u128 * total as u128).div_ceil(counted.max(1) as u128)
    };
    u32::try_from(tokens).unwrap_or(u32::MAX)
}

/// Tokens in `bytes`, decoded lossily. The count is cached under `rev`.
pub fn count_cached(rev: &Revision, bytes: &[u8]) -> u32 {
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().expect("token cache").get(rev) {
        return *hit;
    }
    let n = count(&String::from_utf8_lossy(bytes));
    cache.lock().expect("token cache").insert(rev.clone(), n);
    n
}

/// The first `max` tokens of `text` and how many tokens the text holds, counted as [`count`]
/// does. A cut inside a multi-byte character decodes lossily. Past the analysis bound the cut
/// falls at that byte at the latest.
pub fn take(text: &str, max: u32) -> (String, u32) {
    let head = analyzed(text);
    let tokens = encoder().encode_ordinary(head);
    let total = scale(tokens.len(), head.len(), text.len());
    if total <= max {
        return (text.to_owned(), total);
    }
    let kept = &tokens[..tokens.len().min(max as usize)];
    let bytes = encoder().decode_bytes(kept).unwrap_or_default();
    (String::from_utf8_lossy(&bytes).into_owned(), total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::md::MAX_ANALYZED_BYTES;

    #[test]
    fn counts_match_o200k_base() {
        // "hello world" is two tokens in o200k_base.
        assert_eq!(count("hello world"), 2);
        assert_eq!(count(""), 0);
    }

    #[test]
    fn a_cached_count_is_keyed_by_revision() {
        let rev = Revision::of(b"hello world");
        assert_eq!(count_cached(&rev, b"hello world"), 2);
        // Same revision, so the cached value wins even for different bytes.
        assert_eq!(count_cached(&rev, b"a much longer text than before"), 2);
    }

    #[test]
    fn a_text_over_the_exact_bound_is_estimated_from_its_head() {
        let line = "plain text line without imports\n";
        let head = line.repeat(MAX_ANALYZED_BYTES / line.len());
        let exact = count(&head);
        // Three times the text: the head's count scaled by three, within a token of rounding.
        let long = head.repeat(3);
        let estimate = count(&long);
        assert!(
            estimate.abs_diff(3 * exact) <= 3,
            "{estimate} vs {}",
            3 * exact
        );
        let (kept, total) = take(&long, 10);
        assert_eq!((count(&kept), total), (10, estimate));
    }

    #[test]
    fn take_keeps_a_token_prefix() {
        let (kept, total) = take("hello world", 1);
        assert_eq!((kept.as_str(), total), ("hello", 2));
    }
}
