//! M0: file-transfer task model. This crate does not connect to peers yet.
//! Network connectivity must eventually be supplied exclusively by p2p-sdk.

pub mod task;
pub mod protocol;

pub use task::{Task, TaskError, TaskId, TaskPhase};
