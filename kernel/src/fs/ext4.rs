//! Minimal read-only ext4 filesystem parser.
//!
//! Reads superblock, group descriptors, inodes, and directory entries
//! to support open + read of files. Uses a block read callback to
//! access the underlying block device.

/// ext4 superblock magic.
const EXT4_SUPER_MAGIC: u16 = 0xEF53;

/// Inode number for root directory.
const EXT4_ROOT_INO: u32 = 2;

/// File type in directory entry: regular file.
const EXT4_FT_REG_FILE: u8 = 1;
/// File type in directory entry: directory.
const EXT4_FT_DIR: u8 = 2;

/// On-disk superblock (partial — fields we need).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Superblock {
    pub s_inodes_count: u32,
    pub s_blocks_count_lo: u32,
    pub s_r_blocks_count_lo: u32,
    pub s_free_blocks_count_lo: u32,
    pub s_free_inodes_count: u32,
    pub s_first_data_block: u32,
    pub s_log_block_size: u32,
    pub s_log_cluster_size: u32,
    pub s_blocks_per_group: u32,
    pub s_clusters_per_group: u32,
    pub s_inodes_per_group: u32,
    pub s_mtime: u32,
    pub s_wtime: u32,
    pub s_mnt_count: u16,
    pub s_max_mnt_count: u16,
    pub s_magic: u16,
}

impl Superblock {
    pub fn block_size(&self) -> usize {
        1024 << self.s_log_block_size
    }
}

/// On-disk group descriptor (32 bytes, no 64-bit extensions).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GroupDesc {
    pub bg_block_bitmap_lo: u32,
    pub bg_inode_bitmap_lo: u32,
    pub bg_inode_table_lo: u32,
    pub bg_free_blocks_count_lo: u16,
    pub bg_free_inodes_count_lo: u16,
    pub bg_used_dirs_count_lo: u16,
    pub bg_flags: u16,
    pub bg_exclude_bitmap_lo: u32,
    pub bg_block_bitmap_csum_lo: u16,
    pub bg_inode_bitmap_csum_lo: u16,
    pub bg_itable_unused_lo: u16,
    pub bg_checksum: u16,
}

/// On-disk inode (128 bytes minimum).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Inode {
    pub i_mode: u16,
    pub i_uid: u16,
    pub i_size_lo: u32,
    pub i_atime: u32,
    pub i_ctime: u32,
    pub i_mtime: u32,
    pub i_dtime: u32,
    pub i_gid: u16,
    pub i_links_count: u16,
    pub i_blocks_lo: u32,
    pub i_flags: u32,
    pub i_osd1: u32,
    pub i_block: [u32; 15],
    pub i_generation: u32,
    pub i_file_acl_lo: u32,
    pub i_size_high: u32,
    pub i_obso_faddr: u32,
    pub i_osd2: [u8; 12],
}

impl Inode {
    pub fn size(&self) -> u64 {
        self.i_size_lo as u64 | ((self.i_size_high as u64) << 32)
    }

    /// Check if this inode is a directory.
    pub fn is_dir(&self) -> bool {
        (self.i_mode & 0xF000) == 0x4000
    }

    /// Check if this inode is a regular file.
    pub fn is_file(&self) -> bool {
        (self.i_mode & 0xF000) == 0x8000
    }
}

/// On-disk directory entry (variable length).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DirEntry {
    pub inode: u32,
    pub rec_len: u16,
    pub name_len: u8,
    pub file_type: u8,
    // name bytes follow (up to 255)
}

/// Block read function type: reads `block_no` into `buf`.
/// `buf` is guaranteed to be at least `block_size` bytes.
pub type BlockReadFn = fn(block_no: u64, buf: &mut [u8]) -> Result<(), ()>;

/// Ext4 filesystem handle.
pub struct Ext4Fs {
    sb: Superblock,
    gd: GroupDesc,
    block_size: usize,
    inode_size: usize,
    read_block: BlockReadFn,
}

impl Ext4Fs {
    /// Mount an ext4 filesystem using the given block read function.
    /// Reads superblock from block 1 (offset 1024) and group descriptor.
    pub fn mount(read_block: BlockReadFn) -> Result<Self, &'static str> {
        // Read superblock: always at byte offset 1024
        let mut buf = [0u8; 1024];
        // For 1024-byte blocks, superblock is in block 1.
        // For larger blocks, it's at offset 1024 within block 0.
        // Read block 0 first to check, then block 1.
        read_block(1, &mut buf).map_err(|_| "failed to read superblock block")?;

        let sb: Superblock = unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const Superblock) };

        if sb.s_magic != EXT4_SUPER_MAGIC {
            // Maybe block size > 1024, superblock at offset 1024 in block 0
            read_block(0, &mut buf).map_err(|_| "failed to read block 0")?;
            // Can't handle >1024 block size with 1024-byte buffer for mount
            return Err("bad superblock magic");
        }

        let block_size = sb.block_size();
        // Read inode size from superblock (at offset 88 in the raw superblock)
        // We already have the raw bytes in buf
        let inode_size = u16::from_le_bytes([buf[88], buf[89]]) as usize;
        let inode_size = if inode_size == 0 { 128 } else { inode_size };

        // Read group descriptor table (block after superblock)
        let gd_block = sb.s_first_data_block + 1;
        let mut gd_buf = [0u8; 1024];
        read_block(gd_block as u64, &mut gd_buf).map_err(|_| "failed to read GDT")?;

        let gd: GroupDesc = unsafe { core::ptr::read_unaligned(gd_buf.as_ptr() as *const GroupDesc) };

        Ok(Ext4Fs {
            sb,
            gd,
            block_size,
            inode_size,
            read_block,
        })
    }

    /// Read an inode by number (1-based).
    pub fn read_inode(&self, ino: u32) -> Result<Inode, &'static str> {
        let idx = (ino - 1) as usize;
        let inodes_per_block = self.block_size / self.inode_size;
        let block_offset = idx / inodes_per_block;
        let offset_in_block = (idx % inodes_per_block) * self.inode_size;

        let table_block = self.gd.bg_inode_table_lo as u64 + block_offset as u64;
        let mut buf = [0u8; 1024];
        (self.read_block)(table_block, &mut buf).map_err(|_| "failed to read inode block")?;

        let inode: Inode = unsafe {
            core::ptr::read_unaligned(buf[offset_in_block..].as_ptr() as *const Inode)
        };
        Ok(inode)
    }

    /// Look up a file by path (e.g., "/hello.txt"). Returns inode number.
    pub fn lookup(&self, path: &str) -> Result<u32, &'static str> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return Ok(EXT4_ROOT_INO);
        }

        let mut current_ino = EXT4_ROOT_INO;

        for component in path.split('/') {
            if component.is_empty() {
                continue;
            }
            let inode = self.read_inode(current_ino)?;
            if !inode.is_dir() {
                return Err("not a directory");
            }
            current_ino = self.find_in_dir(&inode, component)?;
        }

        Ok(current_ino)
    }

    /// Find a name in a directory inode. Returns the inode number.
    fn find_in_dir(&self, dir_inode: &Inode, name: &str) -> Result<u32, &'static str> {
        let name_bytes = name.as_bytes();
        // Walk direct blocks (i_block[0..11])
        for i in 0..12 {
            let blk = dir_inode.i_block[i];
            if blk == 0 {
                break;
            }
            let mut buf = [0u8; 1024];
            (self.read_block)(blk as u64, &mut buf).map_err(|_| "failed to read dir block")?;

            let mut off = 0usize;
            while off < self.block_size && off < 1024 {
                if off + 8 > 1024 {
                    break;
                }
                let de: DirEntry = unsafe {
                    core::ptr::read_unaligned(buf[off..].as_ptr() as *const DirEntry)
                };
                if de.rec_len == 0 {
                    break;
                }
                if de.inode != 0 && de.name_len as usize == name_bytes.len() {
                    let de_name = &buf[off + 8..off + 8 + de.name_len as usize];
                    if de_name == name_bytes {
                        return Ok(de.inode);
                    }
                }
                off += de.rec_len as usize;
            }
        }
        Err("file not found")
    }

    /// Read file contents into `buf`. Returns bytes read.
    pub fn read_file(&self, ino: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
        let inode = self.read_inode(ino)?;
        if !inode.is_file() {
            return Err("not a regular file");
        }

        let file_size = inode.size() as usize;
        let to_read = buf.len().min(file_size);
        let mut total = 0usize;

        // Read direct blocks (i_block[0..11])
        for i in 0..12 {
            if total >= to_read {
                break;
            }
            let blk = inode.i_block[i];
            if blk == 0 {
                break;
            }
            let mut block_buf = [0u8; 1024];
            (self.read_block)(blk as u64, &mut block_buf)
                .map_err(|_| "failed to read file block")?;

            let chunk = (to_read - total).min(self.block_size);
            buf[total..total + chunk].copy_from_slice(&block_buf[..chunk]);
            total += chunk;
        }

        Ok(total)
    }

    /// Block size of this filesystem.
    pub fn block_size(&self) -> usize {
        self.block_size
    }
}
