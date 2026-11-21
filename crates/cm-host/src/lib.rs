//! `cm-host` holds the effects: it implements [`Api`] over `cm-core`. The host keeps no state of
//! record. A scan is a function of the filesystem and the environment. It holds two caches: the
//! paths the last scan probed, per `cwd`, which any scan rebuilds, and the two traces opened last,
//! which every use checks against the file.

mod apply;
mod fsio;
mod history;
mod host;
mod recovery;
mod runner;
mod traces;

pub use apply::{OsTrash, Trasher};
pub use history::History;
pub use host::{Host, HostConfig, default_data_dir};
pub use recovery::{OpState, Report, classify, inspect};

#[cfg(test)]
mod boundary_tests;
#[cfg(test)]
mod codex_tests;
#[cfg(test)]
mod e2e;
#[cfg(test)]
mod pipeline_tests;

#[cfg(test)]
mod claude_planner_tests;
#[cfg(test)]
mod gemini_e2e_tests;
