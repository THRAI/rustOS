//! Three-layer file descriptor model (BSD-style).
//!
//! Layer 1: FdTable — per-process Vec<Option<(Arc<FileDescription>, FdFlags)>>
//! Layer 2: FileDescription — shared across dup/fork, holds offset + status flags + FileObject
//! Layer 3: FileObject — enum of Vnode | Pipe{Read,Write} | Device

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::{
    fs::{Pipe, Vnode},
    hal_common::Errno,
};

/// Maximum number of file descriptors per process.
const MAX_FDS: usize = 256;

// ---- FdFlags (per-slot) ----

bitflags::bitflags! {
    /// Per-fd-slot flags (not shared across dup/fork).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FdFlags: u32 {
        const CLOEXEC = 0o2000000;
    }
}

// ---- OpenFlags (status flags on FileDescription) ----

/// Open file status flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags {
    pub read: bool,
    pub write: bool,
    pub append: bool,
}

impl OpenFlags {
    pub const RDONLY: Self = Self {
        read: true,
        write: false,
        append: false,
    };
    pub const WRONLY: Self = Self {
        read: false,
        write: true,
        append: false,
    };
    pub const RDWR: Self = Self {
        read: true,
        write: true,
        append: false,
    };
}

// ---- DeviceKind ----

/// Static device types for /dev nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Null,
    Zero,
    ConsoleRead,
    ConsoleWrite,
    Rtc,
}

// ---- FileObject (Layer 3) ----

/// The backing object behind an open file description.
pub enum FileObject {
    /// Regular file backed by a vnode (ext4, etc.)
    Vnode(Arc<dyn Vnode>),
    /// Read end of a pipe.
    PipeRead(Arc<Pipe>),
    /// Write end of a pipe.
    PipeWrite(Arc<Pipe>),
    /// Static device node.
    Device(DeviceKind),
}

impl Drop for FileObject {
    fn drop(&mut self) {
        match self {
            FileObject::PipeRead(pipe) => pipe.close_read(),
            FileObject::PipeWrite(pipe) => pipe.close_write(),
            _ => {},
        }
    }
}

impl FileObject {
    /// Poll readiness for this file object (used by ppoll).
    ///
    /// Returns `(readable, writable, hangup, error)`.
    /// Pure query — no IO, no blocking.
    pub fn poll_ready(&self, events_in: i16, events_out: i16) -> i16 {
        const POLLIN: i16 = 0x001;
        const POLLOUT: i16 = 0x004;
        const POLLERR: i16 = 0x008;
        const POLLHUP: i16 = 0x010;

        let mut revents: i16 = 0;
        match self {
            FileObject::Device(DeviceKind::ConsoleRead) => {
                if events_in & POLLIN != 0 {
                    revents |= POLLIN;
                }
            },
            FileObject::Device(DeviceKind::ConsoleWrite) => {
                if events_out & POLLOUT != 0 {
                    revents |= POLLOUT;
                }
            },
            FileObject::Device(DeviceKind::Null | DeviceKind::Zero | DeviceKind::Rtc) => {
                revents |= events_in & (POLLIN | POLLOUT);
            },
            FileObject::PipeRead(pipe) => {
                if pipe.readable_len() > 0 {
                    revents |= POLLIN;
                }
                if pipe.is_writer_closed() {
                    revents |= POLLHUP;
                }
            },
            FileObject::PipeWrite(pipe) => {
                if pipe.is_reader_closed() {
                    revents |= POLLERR;
                } else if events_out & POLLOUT != 0 {
                    revents |= POLLOUT;
                }
            },
            FileObject::Vnode(_) => {
                revents |= events_in & (POLLIN | POLLOUT);
            },
        }
        revents
    }
}

// ---- FileDescription (Layer 2) ----

/// An open file description, shared across dup() and fork().
pub struct FileDescription {
    pub object: FileObject,
    pub offset: AtomicU64,
    pub flags: OpenFlags,
    /// fcntl(F_SETFL) managed status bits (shared by duplicated fds).
    pub status_flags: AtomicU32,
}

impl FileDescription {
    pub const O_APPEND: u32 = 0x0000_0400;
    pub const O_NONBLOCK: u32 = 0x0000_0800;

    pub fn new(object: FileObject, flags: OpenFlags) -> Arc<Self> {
        let mut status = 0u32;
        if flags.append {
            status |= Self::O_APPEND;
        }
        Arc::new(Self {
            object,
            offset: AtomicU64::new(0),
            flags,
            status_flags: AtomicU32::new(status),
        })
    }

    pub fn get_status_flags(&self) -> u32 {
        self.status_flags.load(Ordering::Relaxed)
    }

    pub fn set_status_flags(&self, flags: u32) {
        self.status_flags.store(flags, Ordering::Relaxed);
    }

    pub fn is_append(&self) -> bool {
        (self.get_status_flags() & Self::O_APPEND) != 0
    }

    /// Seek: reposition file offset.
    ///
    /// Returns the new absolute offset.  Rejects pipes and devices
    /// with `Espipe`.
    pub fn seek(&self, offset: i64, whence: u32) -> Result<u64, crate::hal_common::Errno> {
        use core::sync::atomic::Ordering;

        use crate::hal_common::Errno;

        const SEEK_SET: u32 = 0;
        const SEEK_CUR: u32 = 1;
        const SEEK_END: u32 = 2;

        match &self.object {
            FileObject::PipeRead(_) | FileObject::PipeWrite(_) => return Err(Errno::Espipe),
            FileObject::Device(_) => return Err(Errno::Espipe),
            FileObject::Vnode(_) => {},
        }

        let size = match &self.object {
            FileObject::Vnode(v) => v.size(),
            _ => 0,
        };

        let cur = self.offset.load(Ordering::Relaxed) as i64;
        let new_off = match whence {
            SEEK_SET => offset,
            SEEK_CUR => cur + offset,
            SEEK_END => size as i64 + offset,
            _ => return Err(Errno::Einval),
        };

        if new_off < 0 {
            return Err(Errno::Einval);
        }

        self.offset.store(new_off as u64, Ordering::Relaxed);
        Ok(new_off as u64)
    }
}

// ---- FdTable (Layer 1) ----

/// Per-process file descriptor table.
pub struct FdTable {
    entries: Vec<Option<(Arc<FileDescription>, FdFlags)>>,
}

impl FdTable {
    /// Create an empty fd table.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Create fd table with stdio: fd 0=ConsoleRead, fd 1/2=ConsoleWrite.
    pub fn new_with_stdio() -> Self {
        let stdin = FileDescription::new(
            FileObject::Device(DeviceKind::ConsoleRead),
            OpenFlags::RDONLY,
        );
        let stdout = FileDescription::new(
            FileObject::Device(DeviceKind::ConsoleWrite),
            OpenFlags::WRONLY,
        );
        let stderr = FileDescription::new(
            FileObject::Device(DeviceKind::ConsoleWrite),
            OpenFlags::WRONLY,
        );
        let entries = alloc::vec![
            Some((stdin, FdFlags::empty())),
            Some((stdout, FdFlags::empty())),
            Some((stderr, FdFlags::empty())),
        ];
        Self { entries }
    }

    /// Allocate the lowest available fd for the given file description.
    pub fn insert(&mut self, desc: Arc<FileDescription>, fd_flags: FdFlags) -> Result<u32, Errno> {
        self.insert_from(0, desc, fd_flags)
    }

    /// Allocate the lowest available fd >= `min_fd`.
    pub fn insert_from(
        &mut self,
        min_fd: u32,
        desc: Arc<FileDescription>,
        fd_flags: FdFlags,
    ) -> Result<u32, Errno> {
        let start = min_fd as usize;
        if start >= MAX_FDS {
            return Err(Errno::Ebadf);
        }
        if self.entries.len() < start {
            self.entries.resize(start, None);
        }
        for (i, slot) in self.entries.iter_mut().enumerate().skip(start) {
            if slot.is_none() {
                *slot = Some((desc, fd_flags));
                return Ok(i as u32);
            }
        }
        let fd = self.entries.len().max(start);
        if fd >= MAX_FDS {
            return Err(Errno::Emfile);
        }
        if self.entries.len() < fd {
            self.entries.resize(fd, None);
        }
        self.entries.push(Some((desc, fd_flags)));
        Ok(fd as u32)
    }

    /// Insert at a specific fd number (for dup2/pipe2). Closes existing if occupied.
    pub fn insert_at(
        &mut self,
        fd: u32,
        desc: Arc<FileDescription>,
        fd_flags: FdFlags,
    ) -> Result<(), Errno> {
        let idx = fd as usize;
        if idx >= MAX_FDS {
            return Err(Errno::Ebadf);
        }
        while self.entries.len() <= idx {
            self.entries.push(None);
        }
        self.entries[idx] = Some((desc, fd_flags));
        Ok(())
    }

    /// Look up a file description by fd.
    pub fn get(&self, fd: u32) -> Option<&Arc<FileDescription>> {
        self.entries
            .get(fd as usize)?
            .as_ref()
            .map(|(desc, _)| desc)
    }

    /// Get fd flags for a slot.
    pub fn get_flags(&self, fd: u32) -> Option<FdFlags> {
        self.entries.get(fd as usize)?.as_ref().map(|(_, f)| *f)
    }

    /// Update fd flags for a slot.
    pub fn set_flags(&mut self, fd: u32, flags: FdFlags) -> Result<(), Errno> {
        let slot = self.entries.get_mut(fd as usize).ok_or(Errno::Ebadf)?;
        let (desc, _) = slot.as_ref().ok_or(Errno::Ebadf)?;
        *slot = Some((Arc::clone(desc), flags));
        Ok(())
    }

    /// Remove (close) an fd. Returns the FileDescription if it existed.
    pub fn remove(&mut self, fd: u32) -> Option<Arc<FileDescription>> {
        let slot = self.entries.get_mut(fd as usize)?;
        slot.take().map(|(desc, _)| desc)
    }

    /// dup: clone Arc<FileDescription> into lowest free fd, clear CLOEXEC.
    pub fn dup(&mut self, old_fd: u32) -> Result<u32, Errno> {
        let desc = Arc::clone(self.get(old_fd).ok_or(Errno::Ebadf)?);
        self.insert(desc, FdFlags::empty())
    }

    /// dup2: clone into specific fd. If new_fd == old_fd, return success.
    pub fn dup2(&mut self, old_fd: u32, new_fd: u32) -> Result<u32, Errno> {
        if old_fd == new_fd {
            let _ = self.get(old_fd).ok_or(Errno::Ebadf)?;
            return Ok(new_fd);
        }
        let desc = Arc::clone(self.get(old_fd).ok_or(Errno::Ebadf)?);
        self.insert_at(new_fd, desc, FdFlags::empty())?;
        Ok(new_fd)
    }

    /// dup3: like dup2 but old_fd != new_fd required, set CLOEXEC if requested.
    pub fn dup3(&mut self, old_fd: u32, new_fd: u32, cloexec: bool) -> Result<u32, Errno> {
        if old_fd == new_fd {
            return Err(Errno::Einval);
        }
        let desc = Arc::clone(self.get(old_fd).ok_or(Errno::Ebadf)?);
        let flags = if cloexec {
            FdFlags::CLOEXEC
        } else {
            FdFlags::empty()
        };
        self.insert_at(new_fd, desc, flags)?;
        Ok(new_fd)
    }

    /// Fork: clone the fd table (Arc-shared FileDescriptions per POSIX).
    pub fn fork(&self) -> Self {
        Self {
            entries: self.entries.clone(),
        }
    }

    /// Strip CLOEXEC fds (called after exec point-of-no-return).
    pub fn strip_cloexec(&mut self) {
        for slot in self.entries.iter_mut() {
            if let Some((_, flags)) = slot {
                if flags.contains(FdFlags::CLOEXEC) {
                    *slot = None;
                }
            }
        }
    }

    /// Perform fcntl operations.
    ///
    /// Pure fd-table logic — no IO.
    pub fn fcntl(
        &mut self,
        fd: u32,
        cmd: u32,
        arg: usize,
    ) -> Result<usize, crate::hal_common::Errno> {
        use crate::hal_common::Errno;

        const F_DUPFD: u32 = 0;
        const F_GETFD: u32 = 1;
        const F_SETFD: u32 = 2;
        const F_GETFL: u32 = 3;
        const F_SETFL: u32 = 4;
        const F_DUPFD_CLOEXEC: u32 = 1030;
        const O_APPEND: usize = 0x0000_0400;
        const O_NONBLOCK: usize = 0x0000_0800;

        match cmd {
            F_DUPFD | F_DUPFD_CLOEXEC => {
                let desc = alloc::sync::Arc::clone(self.get(fd).ok_or(Errno::Ebadf)?);
                let flags = if cmd == F_DUPFD_CLOEXEC {
                    FdFlags::CLOEXEC
                } else {
                    FdFlags::empty()
                };
                let new_fd = self.insert_from(arg as u32, desc, flags)?;
                Ok(new_fd as usize)
            },
            F_GETFD => {
                let flags = self.get_flags(fd).ok_or(Errno::Ebadf)?;
                Ok(if flags.contains(FdFlags::CLOEXEC) {
                    1
                } else {
                    0
                })
            },
            F_SETFD => {
                let _ = self.get(fd).ok_or(Errno::Ebadf)?;
                let new_flags = if (arg & 1) != 0 {
                    FdFlags::CLOEXEC
                } else {
                    FdFlags::empty()
                };
                self.set_flags(fd, new_flags)?;
                Ok(0)
            },
            F_GETFL => {
                let desc = self.get(fd).ok_or(Errno::Ebadf)?;
                let mut fl: usize = 0;
                if desc.flags.read && desc.flags.write {
                    fl = 2; // O_RDWR
                } else if desc.flags.write {
                    fl = 1; // O_WRONLY
                }
                let status = desc.get_status_flags() as usize;
                fl |= status & (O_APPEND | O_NONBLOCK);
                Ok(fl)
            },
            F_SETFL => {
                let desc = self.get(fd).ok_or(Errno::Ebadf)?;
                let settable_mask = (O_APPEND | O_NONBLOCK) as u32;
                let cur = desc.get_status_flags();
                let next = (cur & !settable_mask) | ((arg as u32) & settable_mask);
                desc.set_status_flags(next);
                Ok(0)
            },
            _ => Err(Errno::Einval),
        }
    }
}
