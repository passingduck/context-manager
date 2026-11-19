//! Format-preserving edits of agent configuration files. Each function takes text and returns
//! text. Nothing here touches the disk.

pub mod jsonc;
pub mod toml;
