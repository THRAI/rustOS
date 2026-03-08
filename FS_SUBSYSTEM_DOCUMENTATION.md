# rustOS 文件系统子系统说明（基于当前代码）

本文档基于当前仓库代码整理，重点覆盖 `kernel/src/fs` 目录，并补充其与 `syscall/proc/mm` 的关键交互。

## 1. 总体结论

当前 rustOS 文件系统是一个“**轻量 VFS 外壳 + 单线程 delegate + lwext4 后端**”架构：

1. 系统调用层在 `syscall/fs.rs` 做 Linux 语义适配（open/read/write/mount/pipe/link/rename/getdents 等）。
2. `fs/path.rs + fs/dentry.rs + fs/mount.rs` 负责命名空间解析与缓存。
3. 所有真正 ext4 I/O 都通过 `fs/delegate.rs` 的单任务串行执行，避免 lwext4 并发问题。
4. `fs/ext4.rs + fs/lwext4_disk.rs` 负责把请求落到 lwext4 与 VirtIO-blk。
5. `fs/page_cache.rs + mm/vm/vm_object.rs` 形成页缓存/缺页读取链路（包含 exec 与文件映射路径）。

这套设计的核心 tradeoff 是：并发安全和实现复杂度可控，但吞吐上限受“单 delegate”限制。

## 2. 分层框架

### 2.1 逻辑分层

```text
User Syscall
  -> kernel/src/syscall/fs.rs
     -> fs/path.rs (路径解析)
     -> fs/dentry.rs (目录项缓存)
     -> fs/mount.rs (挂载前缀映射)
     -> fs/fd_table.rs (fd/description/object)
     -> fs/page_cache.rs (页缓存状态机)
     -> fs/delegate.rs (异步请求队列 + 单线程执行)
        -> fs/ext4.rs (lwext4 API 封装)
           -> fs/lwext4_disk.rs (KernelDevOp<Disk>)
              -> drivers/virtio_blk.rs (块设备)
```

### 2.2 启动初始化顺序

`kernel/src/main.rs` 中相关顺序是：

1. `fs::dentry::init()`
2. `fs::page_cache::init()`
3. `fs::delegate::init()`（内部会 mount ext4 并启动 delegate task）

这意味着：dentry/page cache 先可用，再启动后端 I/O 执行体。

## 3. fs 目录逐文件说明

## 3.1 `kernel/src/fs/mod.rs`

作用：文件系统子模块聚合入口。  
导出模块：

1. `ext4`
2. `lwext4_disk`
3. `delegate`
4. `vnode`
5. `dentry`
6. `page_cache`
7. `fd_table`
8. `path`
9. `pipe`
10. `devfs`
11. `mount`
12. `symlink`

并 `pub use vnode::Vnode;`。

## 3.2 `kernel/src/fs/vnode.rs`

作用：定义 VFS 抽象对象 `Vnode` 与 ext4 实现 `Ext4Vnode`。  
关键点：

1. `VnodeType` 目前只有 `Regular/Directory`。
2. `Vnode` trait 提供 `vnode_id()/vtype()/size()/path()/set_size()/grab_obj()`。
3. `Ext4Vnode` 内含：
   `ino`、`vtype`、`file_size(AtomicU64)`、`path`、`vm_obj`。
4. `grab_obj()` 按需创建 file-backed `VmObject`（与 VM 子系统联动）。

## 3.3 `kernel/src/fs/dentry.rs`

作用：全局目录项缓存。  
数据结构：`BTreeMap<(parent_vnode_id, name), Arc<dyn Vnode>>`。  
接口：

1. `init()`
2. `lookup(parent_id, name)`
3. `insert(parent_id, name, vnode)`
4. `invalidate(parent_id, name)`

特点：当前无淘汰策略，适合比赛/实验场景。

## 3.4 `kernel/src/fs/path.rs`

作用：绝对路径解析器。  
核心流程：

1. 从 root vnode 开始迭代组件。
2. 先查 dentry cache。
3. miss 时走 `delegate::fs_lookup()`。
4. 命中后构建 `Ext4Vnode::new_with_path(...)` 并回填 dentry。

细节：

1. 内部维护 `built_path`（逐层拼接完整路径）发给 delegate。
2. `resolve()` 仅处理绝对路径语义（调用方在 syscall 层先做 absolutize）。

## 3.5 `kernel/src/fs/mount.rs`

作用：最小挂载命名空间表（不是完整 VFS mount 实体）。  
关键能力：

1. 挂载项登记/注销：`register_mount` / `unregister_mount`
2. 最长前缀匹配路径映射：`resolve_to_source`
3. 挂载域判断：`same_mount_domain`（用于 link/rename 跨挂载拒绝）

当前模型：仍是单 ext4 后端，通过路径映射模拟 mount namespace。

## 3.6 `kernel/src/fs/page_cache.rs`

作用：页缓存状态机（按 `(vnode_id, page_offset)` 索引）。  
状态：

1. `Absent`（隐含）
2. `Fetching(Vec<Waker>)`
3. `Cached(PhysAddr)`

关键 API：

1. `lookup(...) -> Hit/InitiateFetch/WaitingOnFetch`
2. `complete(...)`（写入 Cached 并唤醒等待者）
3. `probe(...)`
4. `invalidate_range(...)`
5. `invalidate_all(...)`

设计点：通过 `Fetching` 聚合并发缺页，避免重复 I/O。

## 3.7 `kernel/src/fs/fd_table.rs`

作用：3 层 fd 模型实现。

1. Layer1：`FdTable`（每进程 fd 槽）
2. Layer2：`FileDescription`（dup/fork 共享，含 offset/status_flags/object）
3. Layer3：`FileObject`（Vnode/PipeRead/PipeWrite/Device）

能力：

1. `insert/insert_from/insert_at`
2. `dup/dup2/dup3`
3. `fork()`（浅拷贝 entries，Arc 共享 description）
4. `strip_cloexec()`（exec 时清理）

限制：`MAX_FDS = 256`。

## 3.8 `kernel/src/fs/pipe.rs`

作用：pipe 实现（4KB ring buffer + waker）。  
语义：

1. `PIPE_BUF=4096` 内保证原子写（空间不足直接 `EAGAIN`）
2. reader 关闭后 writer 得 `EPIPE`
3. writer 关闭且缓冲空时 reader 返回 EOF（0）

接口：

1. `read()/write()`
2. `register_reader_waker()/register_writer_waker()`
3. `close_read()/close_write()`

## 3.9 `kernel/src/fs/devfs.rs`

作用：静态设备节点 `/dev` 映射（简化模型）。  
当前支持：

1. `null`
2. `zero`
3. `console/tty`

返回 `FileDescription(FileObject::Device(...))`。

## 3.10 `kernel/src/fs/symlink.rs`

作用：内存符号链接表（兼容层）。  
能力：

1. `create/remove/read/resolve`
2. 路径规范化与相对目标展开

现状：主系统调用路径已直接走 ext4 symlink（delegate/ext4），此模块更像备用或过渡实现。

## 3.11 `kernel/src/fs/lwext4_disk.rs`

作用：`KernelDevOp` 适配器，把 lwext4 的块读写请求转到 VirtIO-blk。  
要点：

1. 块大小固定 512B。
2. 支持对齐快路径（整扇区）和非对齐读改写路径。
3. `seek` 支持 `SEEK_SET/SEEK_CUR/SEEK_END`。

## 3.12 `kernel/src/fs/ext4.rs`

作用：lwext4 封装层，暴露 fs 原语。  
关键设计：`DelegateToken`。

1. 所有 I/O API 需要 `&mut DelegateToken`。
2. token 只允许 delegate task 创建，编译期约束“单执行者”。

主要接口：

1. mount：`mount()`
2. 文件：`open/read/write/truncate/close`
3. 元数据：`stat/exists`
4. 目录：`dir_open/dir_next/dir_close/mkdir/unlink`
5. 元操作：`link/rename/symlink/readlink/cache_flush`

## 3.13 `kernel/src/fs/delegate.rs`

作用：FS 中枢调度器。  
这是当前 fs 子系统最核心文件。

机制：

1. 全局请求队列 `REQUEST_QUEUE` + `REQUEST_COUNT`。
2. `FsRequest` 枚举描述所有操作类型。
3. 每类请求用静态 reply pool + `ReplyFuture` 做“轻量 one-shot”。
4. `delegate_task()` 是唯一消费者，串行调用 `ext4::*`。
5. 公开 `fs_open/fs_read/fs_write/...` async API 给外层用。

约束与容量：

1. channel 逻辑容量 `CHANNEL_CAPACITY=256`（当前发送端未硬限检查）。
2. 打开文件槽 `MAX_OPEN_FILES=64`。
3. path 缓冲 256 字节。

重要细节：

1. `map_backend_path()` 每次请求都会调用 `mount::resolve_to_source()`。
2. `FsRequest::Lookup` 的 `parent_ino` 当前未使用，按“完整路径 stat”实现。
3. `FsRequest::Stat` 当前直接返回 `ENOSYS`，实际路径多用 `lookup` 拿 size/type。

## 4. 关键数据流（系统框架如何跑）

## 4.1 open/read/write（普通文件）

`sys_openat_async` -> `absolutize_path` -> `open(...)`：

1. 先尝试 `path::resolve`。
2. 若 `O_CREAT` 且不存在：delegate 先创建再 resolve。
3. 创建 `FileDescription(FileObject::Vnode)` 放入 `FdTable`。
4. `O_TRUNC` 会调用 `delegate::fs_truncate` 并清 `page_cache`。

`sys_read_async`（vnode 分支）：

1. 从 fd 拿到 vnode/path/offset/size。
2. 走 page-cache 缺页路径（`page_cache_fetch_by_id` -> `delegate::fs_read_page`）。
3. copy 到用户态并推进 `desc.offset`。

`sys_write_async`（vnode 分支）：

1. 用户数据拷入内核缓冲。
2. `delegate::fs_write_at(path, offset, data)`。
3. 成功后更新 offset。
4. `page_cache::invalidate_range(...)`，并更新 vnode size。

## 4.2 目录与命名操作

1. `mkdir/unlink/symlink/link/rename/readlink/getdents`
2. 统一在 syscall 层做路径 absolutize 与基本语义检查
3. 再调用 delegate 异步接口
4. unlink 后显式 `dentry::invalidate(...)`
5. `link/rename` 用 `mount::same_mount_domain` 做最小跨挂载限制

## 4.3 mount/umount

当前不是多 superblock 真实挂载切换，而是路径映射：

1. `sys_mount_async` 注册 `MountEntry`
2. 后续 delegate 请求自动 `resolve_to_source`
3. `sys_umount2_async` 删除映射

这提供了命名空间语义，但底层仍是同一 ext4 后端实例。

## 4.4 页错误与 VM 联动

`mm/vm/vm_object.rs` 的 `VnodePager`：

1. `page_in` 调 `delegate::fs_read_page`
2. `page_out` 调 `delegate::fs_write_at`

`proc/exec.rs`：

1. exec 解析 ELF 时通过 `delegate::fs_read_page` 读取头部
2. 并通过 `page_cache::complete(...)` 回填
3. `exec` 后调用 `fd_table.strip_cloexec()`

`proc/fork.rs`：

1. `FdTable::fork()` 共享 `Arc<FileDescription>`
2. cwd 继承

## 5. 现状能力与限制

## 5.1 已具备

1. ext4 基础文件/目录/链接操作
2. dev 节点（null/zero/console）
3. pipe + 异步阻塞语义
4. mount 前缀映射命名空间
5. 页缓存与 VM pager 联动
6. fork/exec 下 fd 语义（含 CLOEXEC）

## 5.2 主要限制

1. delegate 单线程串行，吞吐受限
2. `FsRequest::Stat` 未完成（ENOSYS）
3. mount 仍是路径映射模型，不是完整 VFS 多实例挂载
4. dentry/page cache 无容量淘汰策略
5. 路径缓冲固定 256 字节
6. open file 槽位固定 64

## 6. 快速索引（按需求找文件）

1. 想看“路径怎么解析”：`fs/path.rs`, `syscall/fs.rs`(`absolutize_path`)
2. 想看“fd 语义”：`fs/fd_table.rs`
3. 想看“真正落盘位置”：`fs/delegate.rs` + `fs/ext4.rs` + `fs/lwext4_disk.rs`
4. 想看“页缓存/缺页”：`fs/page_cache.rs`, `mm/vm/vm_object.rs`
5. 想看“pipe”：`fs/pipe.rs`
6. 想看“挂载语义”：`fs/mount.rs`, `syscall/fs.rs` 的 `sys_mount_async/sys_umount2_async`

