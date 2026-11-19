//! `cm-host` holds the effects: it implements [`Api`] over `cm-core`. The host keeps no state of
//! record. A scan is a function of the filesystem and the environment, and the one cache it holds
//! (the paths the last scan probed, per `cwd`) is rebuilt by any scan.

mod apply;
mod fsio;
mod history;
mod host;
mod recovery;

pub use apply::{OsTrash, Trasher};
pub use history::History;
pub use host::{Host, HostConfig, default_data_dir};
pub use recovery::{OpState, Report, classify, inspect};

#[cfg(test)]
mod pipeline_tests;
