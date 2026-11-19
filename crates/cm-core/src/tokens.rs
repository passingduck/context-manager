//! `o200k_base` token counts. The encoder loads once per process. Counts are cached by
//! [`Revision`], so a file that several agents read is tokenized once.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tiktoken_rs::{CoreBPE, o200k_base_singleton};

use crate::model::Revision;

fn encoder() -> &'static CoreBPE {
    o200k_base_singleton()
}

static CACHE: OnceLock<Mutex<HashMap<Revision, u32>>> = OnceLock::new();

/// Tokens in `text`. Special-token strings in the text count as plain text.
pub fn count(text: &str) -> u32 {
    u32::try_from(encoder().encode_ordinary(text).len()).unwrap_or(u32::MAX)
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

/// The first `max` tokens of `text` and how many tokens the text holds. A cut inside a
/// multi-byte character decodes lossily.
pub fn take(text: &str, max: u32) -> (String, u32) {
    let all = encoder().encode_ordinary(text);
    let total = u32::try_from(all.len()).unwrap_or(u32::MAX);
    if total <= max {
        return (text.to_owned(), total);
    }
    let bytes = encoder()
        .decode_bytes(&all[..max as usize])
        .unwrap_or_default();
    (String::from_utf8_lossy(&bytes).into_owned(), total)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn take_keeps_a_token_prefix() {
        let (kept, total) = take("hello world", 1);
        assert_eq!((kept.as_str(), total), ("hello", 2));
    }
}
