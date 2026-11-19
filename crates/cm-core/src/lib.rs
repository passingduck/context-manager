//! `cm-core` reads only: the model, the API table, the adapters and the scan. It contains no
//! code that writes a file.

pub mod agents;
pub mod api;
pub mod edit;
pub mod env;
pub mod model;
pub mod parked;
pub mod probe_fs;
pub mod scan;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use api::{Api, Request, dispatch};
pub use model::*;
