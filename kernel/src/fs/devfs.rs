//! Static device node lookup for /dev/null, /dev/zero, /dev/console.

use alloc::sync::Arc;
use hal_common::Errno;

use super::fd_table::{DeviceKind, FileDescription, FileObject, OpenFlags};

/// Look up a device by path under /dev/.
/// Returns a FileDescription for the device, or ENOENT.
pub fn open_device(name: &str, flags: OpenFlags) -> Result<Arc<FileDescription>, Errno> {
    let kind = match name {
        "null" => DeviceKind::Null,
        "zero" => DeviceKind::Zero,
        "console" | "tty" => {
            if flags.write && !flags.read {
                DeviceKind::ConsoleWrite
            } else if flags.read && !flags.write {
                DeviceKind::ConsoleRead
            } else {
                // RDWR: treat as write (console is line-oriented)
                DeviceKind::ConsoleWrite
            }
        }
        _ => return Err(Errno::ENOENT),
    };
    Ok(FileDescription::new(FileObject::Device(kind), flags))
}
