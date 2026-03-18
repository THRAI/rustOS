//! Physical page lifecycle: VmPage metadata, PageRef RAII handle, busy-lock waker queue.

mod page_ref;
mod vm_page;
pub(crate) mod wait_queue;

pub use page_ref::PageRef;
pub use vm_page::{ExclusiveBusyGuard, SharedBusyGuard, VmPage};
pub use wait_queue::{register_waker, remove_waker, wake_all};
