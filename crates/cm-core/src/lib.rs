//! `cm-core` reads only: the model, the API table, the adapters and the scan. It contains no
//! code that writes a file.

pub mod agents;
pub mod api;
pub(crate) mod cfg;
pub(crate) mod doctor;
pub mod edit;
pub mod env;
pub mod glob;
pub(crate) mod limits;
pub(crate) mod md;
pub mod model;
pub(crate) mod parked;
pub mod passes;
pub mod probe_fs;
pub mod program;
pub mod scan;
pub mod search;
pub mod simulate;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod texts;
pub mod tokens;
pub mod trace;
pub mod walk;

pub use api::{Api, Request, dispatch};
pub use model::*;
