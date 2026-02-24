//! lwext4 disk adapter: implements KernelDevOp for our VirtIO-blk driver.

use lwext4_rust::KernelDevOp;

const BLOCK_SIZE: usize = 512;

/// Cursor-based disk device for lwext4.
pub struct Disk {
    block_id: usize,
    offset: usize,
}

impl Disk {
    pub fn new() -> Self {
        Self {
            block_id: 0,
            offset: 0,
        }
    }

    fn size(&self) -> u64 {
        let blk = crate::drivers::virtio_blk::get();
        blk.lock().capacity() * BLOCK_SIZE as u64
    }

    fn position(&self) -> u64 {
        (self.block_id * BLOCK_SIZE + self.offset) as u64
    }

    fn set_position(&mut self, pos: u64) {
        self.block_id = pos as usize / BLOCK_SIZE;
        self.offset = pos as usize % BLOCK_SIZE;
    }

    fn read_one(&mut self, buf: &mut [u8]) -> Result<usize, i32> {
        let blk = crate::drivers::virtio_blk::get();
        let read_size = if self.offset == 0 && buf.len() >= BLOCK_SIZE {
            let sector_buf: &mut [u8; 512] =
                (&mut buf[..BLOCK_SIZE]).try_into().unwrap();
            blk.lock().read_sector(self.block_id as u64, sector_buf)
                .map_err(|_| -5)?;
            self.block_id += 1;
            BLOCK_SIZE
        } else {
            let mut data = [0u8; BLOCK_SIZE];
            blk.lock().read_sector(self.block_id as u64, &mut data)
                .map_err(|_| -5)?;
            let start = self.offset;
            let count = buf.len().min(BLOCK_SIZE - self.offset);
            buf[..count].copy_from_slice(&data[start..start + count]);
            self.offset += count;
            if self.offset >= BLOCK_SIZE {
                self.block_id += 1;
                self.offset -= BLOCK_SIZE;
            }
            count
        };
        Ok(read_size)
    }

    fn write_one(&mut self, buf: &[u8]) -> Result<usize, i32> {
        let blk = crate::drivers::virtio_blk::get();
        let write_size = if self.offset == 0 && buf.len() >= BLOCK_SIZE {
            let sector_buf: &[u8; 512] =
                (&buf[..BLOCK_SIZE]).try_into().unwrap();
            blk.lock().write_sector(self.block_id as u64, sector_buf)
                .map_err(|_| -5)?;
            self.block_id += 1;
            BLOCK_SIZE
        } else {
            let mut data = [0u8; BLOCK_SIZE];
            blk.lock().read_sector(self.block_id as u64, &mut data)
                .map_err(|_| -5)?;
            let start = self.offset;
            let count = buf.len().min(BLOCK_SIZE - self.offset);
            data[start..start + count].copy_from_slice(&buf[..count]);
            blk.lock().write_sector(self.block_id as u64, &data)
                .map_err(|_| -5)?;
            self.offset += count;
            if self.offset >= BLOCK_SIZE {
                self.block_id += 1;
                self.offset -= BLOCK_SIZE;
            }
            count
        };
        Ok(write_size)
    }
}

impl KernelDevOp for Disk {
    type DevType = Disk;

    fn read(dev: &mut Self, mut buf: &mut [u8]) -> Result<usize, i32> {
        let mut read_len = 0;
        while !buf.is_empty() {
            match dev.read_one(buf) {
                Ok(0) => break,
                Ok(n) => {
                    let tmp = buf;
                    buf = &mut tmp[n..];
                    read_len += n;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(read_len)
    }

    fn write(dev: &mut Self, mut buf: &[u8]) -> Result<usize, i32> {
        let mut write_len = 0;
        while !buf.is_empty() {
            match dev.write_one(buf) {
                Ok(0) => break,
                Ok(n) => {
                    buf = &buf[n..];
                    write_len += n;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(write_len)
    }

    fn flush(_dev: &mut Self) -> Result<usize, i32> {
        Ok(0)
    }

    fn seek(dev: &mut Self, off: i64, whence: i32) -> Result<i64, i32> {
        let size = dev.size();
        let new_pos = match whence as u32 {
            lwext4_rust::bindings::SEEK_SET => Some(off),
            lwext4_rust::bindings::SEEK_CUR => {
                dev.position().checked_add_signed(off).map(|v| v as i64)
            }
            lwext4_rust::bindings::SEEK_END => {
                size.checked_add_signed(off).map(|v| v as i64)
            }
            _ => Some(off),
        }
        .ok_or(-1)?;

        dev.set_position(new_pos as u64);
        Ok(new_pos)
    }
}
