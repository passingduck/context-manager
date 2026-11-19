//! `cm-host` holds the effects: it implements [`Api`] over `cm-core`. The host keeps no state of
//! record. A scan is a function of the filesystem and the environment, and the one cache it holds
//! (the paths the last scan probed, per `cwd`) is rebuilt by any scan.

mod host;

pub use host::{Host, HostConfig};
