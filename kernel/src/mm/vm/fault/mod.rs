//! Page fault handling: synchronous (trap-stack) and asynchronous (yielding).

pub mod async_resolve;
mod sync;

pub use async_resolve::resolve_user_fault;
pub use sync::{sync_fault_handler, FaultResult, PageFaultAccessType};
