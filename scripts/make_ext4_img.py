#!/usr/bin/env python3
"""Create a minimal ext4 filesystem image with /hello.txt.

Works on macOS without mkfs.ext4 or Docker. Writes raw ext4 structures
directly: superblock, group descriptors, bitmaps, inode table, root
directory, and a single file with known content.

Usage: python3 make_ext4_img.py [output_path]
"""

import struct
import sys
import os
import time
import uuid

OUTPUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "test.img")
IMG_SIZE = 32 * 1024 * 1024  # 32 MB
BLOCK_SIZE = 1024
BLOCKS_COUNT = IMG_SIZE // BLOCK_SIZE
INODES_COUNT = 128  # minimal
INODES_PER_GROUP = INODES_COUNT
BLOCKS_PER_GROUP = BLOCKS_COUNT  # single block group
INODE_SIZE = 128
FILE_CONTENT = b"hello from ext4"

# ext4 superblock constants
EXT4_SUPER_MAGIC = 0xEF53
EXT4_FEATURE_COMPAT_EXT_ATTR = 0x0008
EXT4_FEATURE_INCOMPAT_FILETYPE = 0x0002
EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER = 0x0001

# Directory entry file types
EXT4_FT_REG_FILE = 1
EXT4_FT_DIR = 2

def write_at(img, offset, data):
    img[offset:offset+len(data)] = data

def pack_le16(v): return struct.pack('<H', v)
def pack_le32(v): return struct.pack('<I', v)

def main():
    img = bytearray(IMG_SIZE)
    now = int(time.time())
    fs_uuid = uuid.uuid4().bytes

    # --- Layout (single block group, 1024-byte blocks) ---
    # Block 0: boot block (unused, skip)
    # Block 1: superblock
    # Block 2: group descriptor table
    # Block 3: block bitmap
    # Block 4: inode bitmap
    # Block 5..5+(inode_table_blocks-1): inode table
    # Then data blocks for root dir and file

    inode_table_blocks = (INODES_COUNT * INODE_SIZE + BLOCK_SIZE - 1) // BLOCK_SIZE
    first_data_block = 5 + inode_table_blocks  # first usable data block

    # Root dir inode = 2, hello.txt inode = 12 (first non-reserved)
    ROOT_INO = 2
    FILE_INO = 12
    root_dir_block = first_data_block
    file_data_block = first_data_block + 1

    # Number of used blocks: boot + super + gdt + bbitmap + ibitmap + inode_table + root_dir + file_data
    used_blocks = file_data_block + 1
    free_blocks = BLOCKS_COUNT - used_blocks
    # Used inodes: 1..11 reserved + inode 12 (hello.txt) = 12 used
    used_inodes = FILE_INO
    free_inodes = INODES_COUNT - used_inodes

    # ---- Superblock (offset 1024, block 1) ----
    sb = bytearray(BLOCK_SIZE)
    struct.pack_into('<I', sb, 0, INODES_COUNT)          # s_inodes_count
    struct.pack_into('<I', sb, 4, BLOCKS_COUNT)           # s_blocks_count_lo
    struct.pack_into('<I', sb, 8, 0)                      # s_r_blocks_count_lo
    struct.pack_into('<I', sb, 12, free_blocks)           # s_free_blocks_count_lo
    struct.pack_into('<I', sb, 16, free_inodes)           # s_free_inodes_count
    struct.pack_into('<I', sb, 20, 1)                     # s_first_data_block (1 for 1024-byte blocks)
    struct.pack_into('<I', sb, 24, 0)                     # s_log_block_size (0 = 1024)
    struct.pack_into('<I', sb, 28, 0)                     # s_log_cluster_size
    struct.pack_into('<I', sb, 32, BLOCKS_PER_GROUP)      # s_blocks_per_group
    struct.pack_into('<I', sb, 36, BLOCKS_PER_GROUP)      # s_clusters_per_group
    struct.pack_into('<I', sb, 40, INODES_PER_GROUP)      # s_inodes_per_group
    struct.pack_into('<I', sb, 44, now)                    # s_mtime
    struct.pack_into('<I', sb, 48, now)                    # s_wtime
    struct.pack_into('<H', sb, 52, 0)                     # s_mnt_count
    struct.pack_into('<H', sb, 54, 0xFFFF)                # s_max_mnt_count
    struct.pack_into('<H', sb, 56, EXT4_SUPER_MAGIC)      # s_magic
    struct.pack_into('<H', sb, 58, 1)                     # s_state (EXT4_VALID_FS)
    struct.pack_into('<H', sb, 60, 1)                     # s_errors (continue)
    struct.pack_into('<H', sb, 62, 0)                     # s_minor_rev_level
    struct.pack_into('<I', sb, 64, now)                    # s_lastcheck
    struct.pack_into('<I', sb, 68, 0)                     # s_checkinterval
    struct.pack_into('<I', sb, 72, 0)                     # s_creator_os (Linux)
    struct.pack_into('<I', sb, 76, 1)                     # s_rev_level (dynamic)
    struct.pack_into('<H', sb, 80, 0)                     # s_def_resuid
    struct.pack_into('<H', sb, 82, 0)                     # s_def_resgid
    # -- EXT4_DYNAMIC_REV fields --
    struct.pack_into('<I', sb, 84, 11)                    # s_first_ino (first non-reserved)
    struct.pack_into('<H', sb, 88, INODE_SIZE)            # s_inode_size
    struct.pack_into('<H', sb, 90, 0)                     # s_block_group_nr
    struct.pack_into('<I', sb, 92, 0)                     # s_feature_compat
    struct.pack_into('<I', sb, 96, EXT4_FEATURE_INCOMPAT_FILETYPE)  # s_feature_incompat
    struct.pack_into('<I', sb, 100, 0)                    # s_feature_ro_compat
    sb[104:120] = fs_uuid                                  # s_uuid
    # s_volume_name at 120 (16 bytes) - leave zeros
    # s_last_mounted at 136 (64 bytes) - leave zeros

    write_at(img, 1024, sb)

    # ---- Group Descriptor (block 2, offset 2048) ----
    gd = bytearray(32)
    struct.pack_into('<I', gd, 0, 3)                      # bg_block_bitmap_lo (block 3)
    struct.pack_into('<I', gd, 4, 4)                      # bg_inode_bitmap_lo (block 4)
    struct.pack_into('<I', gd, 8, 5)                      # bg_inode_table_lo (block 5)
    struct.pack_into('<H', gd, 12, free_blocks & 0xFFFF)  # bg_free_blocks_count_lo
    struct.pack_into('<H', gd, 14, free_inodes & 0xFFFF)  # bg_free_inodes_count_lo
    struct.pack_into('<H', gd, 16, 1)                     # bg_used_dirs_count_lo (root dir)

    write_at(img, 2 * BLOCK_SIZE, gd)

    # ---- Block Bitmap (block 3) ----
    bb = bytearray(BLOCK_SIZE)
    # Mark blocks 0..used_blocks-1 as used
    for i in range(used_blocks):
        bb[i // 8] |= (1 << (i % 8))
    write_at(img, 3 * BLOCK_SIZE, bb)

    # ---- Inode Bitmap (block 4) ----
    ib = bytearray(BLOCK_SIZE)
    # Mark inodes 1..FILE_INO as used (inode numbers are 1-based, bitmap is 0-based)
    for i in range(FILE_INO):
        ib[i // 8] |= (1 << (i % 8))
    write_at(img, 4 * BLOCK_SIZE, ib)

    # ---- Inode Table (block 5+) ----
    inode_table_off = 5 * BLOCK_SIZE

    # Root directory inode (inode 2, index 1 in table)
    root_inode = bytearray(INODE_SIZE)
    struct.pack_into('<H', root_inode, 0, 0o40755)        # i_mode (directory)
    struct.pack_into('<H', root_inode, 2, 0)              # i_uid
    struct.pack_into('<I', root_inode, 4, BLOCK_SIZE)     # i_size_lo (one block)
    struct.pack_into('<I', root_inode, 8, now)             # i_atime
    struct.pack_into('<I', root_inode, 12, now)            # i_ctime
    struct.pack_into('<I', root_inode, 16, now)            # i_mtime
    struct.pack_into('<I', root_inode, 20, 0)              # i_dtime
    struct.pack_into('<H', root_inode, 24, 0)              # i_gid
    struct.pack_into('<H', root_inode, 26, 3)              # i_links_count (., .., lost+found or just . and ..)
    struct.pack_into('<I', root_inode, 28, 2)              # i_blocks_lo (in 512-byte units: 1024/512=2)
    struct.pack_into('<I', root_inode, 32, 0)              # i_flags
    # i_block[0] = root_dir_block
    struct.pack_into('<I', root_inode, 40, root_dir_block) # i_block[0]

    write_at(img, inode_table_off + 1 * INODE_SIZE, root_inode)

    # hello.txt inode (inode 12, index 11 in table)
    file_inode = bytearray(INODE_SIZE)
    struct.pack_into('<H', file_inode, 0, 0o100644)       # i_mode (regular file)
    struct.pack_into('<H', file_inode, 2, 0)              # i_uid
    struct.pack_into('<I', file_inode, 4, len(FILE_CONTENT))  # i_size_lo
    struct.pack_into('<I', file_inode, 8, now)             # i_atime
    struct.pack_into('<I', file_inode, 12, now)            # i_ctime
    struct.pack_into('<I', file_inode, 16, now)            # i_mtime
    struct.pack_into('<I', file_inode, 20, 0)              # i_dtime
    struct.pack_into('<H', file_inode, 24, 0)              # i_gid
    struct.pack_into('<H', file_inode, 26, 1)              # i_links_count
    struct.pack_into('<I', file_inode, 28, 2)              # i_blocks_lo (1024/512=2)
    struct.pack_into('<I', file_inode, 32, 0)              # i_flags
    # i_block[0] = file_data_block
    struct.pack_into('<I', file_inode, 40, file_data_block) # i_block[0]

    write_at(img, inode_table_off + (FILE_INO - 1) * INODE_SIZE, file_inode)

    # ---- Root Directory Data (block root_dir_block) ----
    dirdata = bytearray(BLOCK_SIZE)
    off = 0

    # Entry: "." -> inode 2
    name = b"."
    rec_len = 12  # minimum: 8 + name_len rounded up to 4
    struct.pack_into('<I', dirdata, off, ROOT_INO)
    struct.pack_into('<H', dirdata, off+4, rec_len)
    struct.pack_into('<B', dirdata, off+6, len(name))
    struct.pack_into('<B', dirdata, off+7, EXT4_FT_DIR)
    dirdata[off+8:off+8+len(name)] = name
    off += rec_len

    # Entry: ".." -> inode 2 (root's parent is itself)
    name = b".."
    rec_len = 12
    struct.pack_into('<I', dirdata, off, ROOT_INO)
    struct.pack_into('<H', dirdata, off+4, rec_len)
    struct.pack_into('<B', dirdata, off+6, len(name))
    struct.pack_into('<B', dirdata, off+7, EXT4_FT_DIR)
    dirdata[off+8:off+8+len(name)] = name
    off += rec_len

    # Entry: "hello.txt" -> inode FILE_INO
    name = b"hello.txt"
    # Last entry: rec_len extends to end of block
    rec_len = BLOCK_SIZE - off
    struct.pack_into('<I', dirdata, off, FILE_INO)
    struct.pack_into('<H', dirdata, off+4, rec_len)
    struct.pack_into('<B', dirdata, off+6, len(name))
    struct.pack_into('<B', dirdata, off+7, EXT4_FT_REG_FILE)
    dirdata[off+8:off+8+len(name)] = name

    write_at(img, root_dir_block * BLOCK_SIZE, dirdata)

    # ---- File Data (block file_data_block) ----
    write_at(img, file_data_block * BLOCK_SIZE, FILE_CONTENT)

    # ---- Write image ----
    with open(OUTPUT, 'wb') as f:
        f.write(img)

    print(f"Created {OUTPUT} ({IMG_SIZE // (1024*1024)} MB ext4 with /hello.txt)")

if __name__ == "__main__":
    main()
