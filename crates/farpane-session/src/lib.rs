//! The split between a privileged service and a session worker (design
//! reference, section 19), without any platform calls.
//!
//! The service owns the network, the accounts and the policy; the worker runs
//! in the user's graphical session, where the screen is. This crate has what
//! both platforms share:
//!
//! - [`ipc`]: the messages between them (frames of changed pixels one way,
//!   input the other) and their framing, with every length bounded;
//! - [`remote`]: the service's side, a `Source` and an `Input` that forward to
//!   whichever worker is attached, across worker restarts;
//! - [`worker`]: the worker's loop over any `Source` and `Input`.
//!
//! How a worker is started and how the pipes are made is platform business
//! (`farpane-win::session` for Windows).

pub mod ipc;
pub mod remote;
pub mod worker;
