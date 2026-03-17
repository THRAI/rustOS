//! Pager trait + concrete implementations (anonymous, vnode-backed).

use alloc::sync::Arc;

use crate::{
    hal_common::{PhysAddr, PAGE_SIZE},
    mm::pmap_zero_page,
};

/// Pager trait for clustered I/O operations (BSD vm_pager interface).
/// Supports fetching multiple pages in a single operation for efficiency.
pub trait Pager: core::fmt::Debug + Send + Sync {
    /// Fetch pages from backing store.
    fn page_in(
        &self,
        offset: usize,
        pa: PhysAddr,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>;
    /// Write pages to backing store.
    ///
    /// `len` is the number of valid bytes in the page to write (may be less
    /// than `PAGE_SIZE` for the last page of a file).
    fn page_out(
        &self,
        offset: usize,
        pa: PhysAddr,
        len: usize,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>;

    /// Returns true if this is an anonymous pager.
    fn is_anon(&self) -> bool {
        false
    }
}

/// Anonymous Pager.
#[derive(Debug)]
pub struct AnonPager;

impl Pager for AnonPager {
    fn is_anon(&self) -> bool {
        true
    }

    fn page_in(
        &self,
        _offset: usize,
        pa: PhysAddr,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>
    {
        alloc::boxed::Box::pin(async move {
            pmap_zero_page(pa);
            Ok(())
        })
    }

    fn page_out(
        &self,
        _offset: usize,
        _pa: PhysAddr,
        _len: usize,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>
    {
        alloc::boxed::Box::pin(async move {
            // Anon pages are never paged out currently.
            Ok(())
        })
    }
}

/// Vnode Pager for file-backed pages.
#[derive(Debug)]
pub struct VnodePager {
    pub vnode_id: usize,
    pub path: alloc::string::String,
    /// File offset that corresponds to object offset 0.
    pub base_offset: usize,
    /// Number of valid file-backed bytes from `base_offset`.
    /// Bytes beyond this range are treated as zero-fill (ELF BSS tail).
    pub valid_bytes: usize,
}

impl Pager for VnodePager {
    fn page_in(
        &self,
        offset: usize,
        pa: PhysAddr,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>
    {
        let path = self.path.clone();
        let base_offset = self.base_offset;
        let valid_bytes = self.valid_bytes;
        alloc::boxed::Box::pin(async move {
            let rel = offset.saturating_sub(base_offset);
            let file_bytes_this_page = if valid_bytes == usize::MAX {
                PAGE_SIZE
            } else if rel >= valid_bytes {
                0
            } else {
                core::cmp::min(PAGE_SIZE, valid_bytes - rel)
            };

            // Pure zero-fill page (beyond p_filesz or zero-file segment).
            if file_bytes_this_page == 0 {
                pmap_zero_page(pa);
                return Ok(());
            }

            crate::fs::fs_read_page(&path, offset as u64, pa)
                .await
                .map_err(|_| ())?;

            // Clamp page tail to zero when it crosses the file-backed boundary.
            if file_bytes_this_page < PAGE_SIZE {
                let buf =
                    unsafe { core::slice::from_raw_parts_mut(pa.as_usize() as *mut u8, PAGE_SIZE) };
                buf[file_bytes_this_page..].fill(0);
            }
            Ok(())
        })
    }

    fn page_out(
        &self,
        offset: usize,
        pa: PhysAddr,
        len: usize,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>
    {
        let path = self.path.clone();
        alloc::boxed::Box::pin(async move {
            let write_len = core::cmp::min(len, PAGE_SIZE);
            let data =
                unsafe { core::slice::from_raw_parts(pa.as_usize() as *const u8, write_len) };
            crate::fs::fs_write_at(&path, offset as u64, data)
                .await
                .map(|_| ())
                .map_err(|_| ())
        })
    }
}
