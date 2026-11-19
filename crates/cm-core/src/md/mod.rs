//! Markdown engines shared by the adapters: frontmatter, spans, imports, references and the
//! text Claude Code strips before a file reaches the model.

pub mod frontmatter;
pub mod imports;
pub mod refs;
pub mod spans;
pub mod strip;
