//! Markdown engines shared by the adapters: frontmatter, spans, imports, references and the
//! text Claude Code strips before a file reaches the model.

pub mod frontmatter;
pub mod imports;
pub mod refs;
pub mod spans;
pub mod strip;

/// The scan analyzes at most this many bytes of one text: it counts their tokens, and it looks
/// for links and mentions in them. A real context file has tens of kilobytes, and Claude Code
/// warns about an instruction file over 40,000 characters. In a release build counting costs
/// about 17 ms per MiB and the span scan about 7 ms, each about twelve times that in a debug
/// build. One scan analyzes every text that reaches the model, so without this bound a single
/// 8 MiB file costs a release scan a quarter of a second for each time it is analyzed.
pub const MAX_ANALYZED_BYTES: usize = 1024 * 1024;

/// Whether the scan analyzes only the first part of a text of `bytes` bytes.
pub fn is_partly_analyzed(bytes: u64) -> bool {
    bytes > MAX_ANALYZED_BYTES as u64
}

/// The part of `text` the scan analyzes: its first [`MAX_ANALYZED_BYTES`], cut at a character
/// boundary.
pub fn analyzed(text: &str) -> &str {
    &text[..text.floor_char_boundary(MAX_ANALYZED_BYTES)]
}
