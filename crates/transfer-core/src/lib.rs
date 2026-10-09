//! M0: file-transfer task model. This crate does not connect to peers yet.
//! Network connectivity must eventually be supplied exclusively by p2p-sdk.

pub mod task;
pub mod protocol;

pub use task::{Task, TaskError, TaskId, TaskPhase};

pub mod access;

pub mod secure_io;

pub mod stream_transfer;

pub mod sdk_quic;

pub mod rpc;

pub mod recursive;
