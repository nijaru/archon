//! Archon control plane: persistent command log, API server, CLI.
//!
//! The server owns a [`NodeService`] and appends every applied kernel
//! command to a JSONL log. On boot it replays the log to rebuild cluster
//! state, then expires leases that were live at shutdown — a restart never
//! re-executes work, it only restores decisions.

pub mod api;
pub mod log;
pub mod server;
