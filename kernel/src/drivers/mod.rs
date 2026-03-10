//! Device drivers.

pub mod virtio_blk;
pub mod virtio_mmio;

pub use virtio_blk::{get as virtio_blk_get, init as init_virtio_blk};
pub use virtio_mmio::*;
