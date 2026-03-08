# rustOS 文件系统子系统深度说明（基于当前代码）

本文档按你当前仓库实现重新整理，重点覆盖 `kernel/src/fs`，并把它和 `syscall/fs.rs`、`proc`、`vm` 的真实调用路径连起来讲。

目标不是“模块名列表”，而是回答这 4 个问题：

1. 每个模块维护了什么数据结构，字段各自承担什么语义。
2. 关键系统调用是怎样一层层下沉到 ext4 的。
3. 现在实现里的关键约束和边界在哪里（并发、容量、语义差异）。
4. 后续扩展时，应该在哪一层动手，哪些地方不能乱改。

---

## 1. 启动与总体框架

### 1.1 启动顺序（为什么是这个顺序）

`kernel/src/main.rs` 的 FS 初始化顺序如下：

```rust
klog!(boot, info, "dentry::init...");
fs::dentry::init();
klog!(boot, info, "page_cache::init...");
fs::page_cache::init();

klog!(boot, info, "delegate::init...");
fs::delegate::init();
```

顺序语义：

1. `dentry` 先初始化，保证路径解析时有缓存容器。
2. `page_cache` 再初始化，保证后续缺页/读路径可以登记状态。
3. 最后启动 `delegate`，由它挂载 ext4 并作为唯一后端执行者。

这说明你的设计核心不是“每个 syscall 直接打 ext4”，而是“所有后端 I/O 统一经过 delegate 串行化”。

### 1.2 分层图（代码真实结构）

```text
syscall/fs.rs
  -> path.rs        (路径绝对化 + vnode 解析)
  -> dentry.rs      (目录项缓存)
  -> mount.rs       (挂载前缀映射)
  -> fd_table.rs    (fd/description/object 三层模型)
  -> pipe.rs/devfs.rs (特殊文件对象)
  -> delegate.rs    (请求队列 + 单任务执行)
     -> ext4.rs     (lwext4 API 封装)
        -> lwext4_disk.rs (KernelDevOp 适配 virtio-blk)
           -> drivers/virtio_blk.rs
```

VM 旁路：

```text
mm/vm/vm_object.rs::VnodePager
  -> delegate::fs_read_page / fs_write_at
```

---

## 2. 核心数据模型（先看这个，后面模块才容易读懂）

## 2.1 委托请求模型：`FsRequest`

`delegate.rs` 里定义了所有可下发给后端任务的操作类型：

```rust
pub enum FsRequest {
    Open { path: [u8; 256], path_len: usize, flags: u32, reply: ReplySlot<Result<FsFileHandle, i32>> },
    Read { handle: FsFileHandle, buf_ptr: usize, len: usize, reply: ReplySlot<Result<usize, i32>> },
    Write { handle: FsFileHandle, buf_ptr: usize, len: usize, reply: ReplySlot<Result<usize, i32>> },
    Close { handle: FsFileHandle, reply: ReplySlot<Result<(), i32>> },
    Lookup { parent_ino: u32, name: [u8; 256], name_len: usize, reply: ReplySlot<Result<(u32, u8, u64), i32>> },
    Stat { ino: u32, reply: ReplySlot<Result<(u64, u8), i32>> },
    ReadPage { path: [u8; 256], path_len: usize, offset: u64, pa: usize, reply: ReplySlot<Result<(), i32>> },
    WriteAt { path: [u8; 256], path_len: usize, offset: u64, data_ptr: usize, data_len: usize, reply: ReplySlot<Result<usize, i32>> },
    Truncate { path: [u8; 256], path_len: usize, size: u64, reply: ReplySlot<Result<(), i32>> },
    Mkdir { path: [u8; 256], path_len: usize, reply: ReplySlot<Result<(), i32>> },
    Unlink { path: [u8; 256], path_len: usize, is_dir: bool, reply: ReplySlot<Result<(), i32>> },
    Link { old_path: [u8; 256], old_len: usize, new_path: [u8; 256], new_len: usize, reply: ReplySlot<Result<(), i32>> },
    Rename { old_path: [u8; 256], old_len: usize, new_path: [u8; 256], new_len: usize, reply: ReplySlot<Result<(), i32>> },
    Symlink { target: [u8; 256], target_len: usize, path: [u8; 256], path_len: usize, reply: ReplySlot<Result<(), i32>> },
    ReadLink { path: [u8; 256], path_len: usize, reply: ReplySlot<Result<(usize, [u8; 256]), i32>> },
    CacheFlush { path: [u8; 256], path_len: usize, reply: ReplySlot<Result<(), i32>> },
    ReadDir { path: [u8; 256], path_len: usize, start_idx: usize, reply: ReplySlot<Result<([DirEntryRaw; READDIR_BATCH], usize), i32>> },
}
```

关键点：

1. API 是“消息协议”而不是函数调用，便于异步和串行化。
2. 请求里路径固定 256 字节，超长路径会被截断，这是当前硬边界。
3. `Read/Write` 传裸指针（`buf_ptr`），要求调用端保证生命周期和内存合法。

## 2.2 Reply 模型：静态池 + Future

`delegate` 没用动态 oneshot channel，而是自己做了静态回复槽：

```rust
struct ReplyInner<T> {
    done: AtomicBool,
    waker: IrqSafeSpinLock<Option<Waker>>,
    value: IrqSafeSpinLock<Option<T>>,
}

struct ReplyFuture<T: Copy + 'static> {
    inner: &'static ReplyInner<T>,
}
```

加上每类请求一个固定池：

```rust
const REPLY_POOL_SIZE: usize = 64;
define_reply_pool!(OPEN_REPLIES, Result<FsFileHandle, i32>);
// ... 其它类型 reply pool
```

语义：

1. `send_request` 后，调用方 await `ReplyFuture`。
2. delegate 完成后 `reply.complete(...)`，写值并 wake。
3. 这是 no_std 场景下的低分配方案，但并发上限被池大小限制。

## 2.3 进程文件对象模型：`FdTable` 三层

`fd_table.rs` 是你当前 VFS 语义最关键的数据结构。

```rust
pub enum FileObject {
    Vnode(Arc<dyn Vnode>),
    PipeRead(Arc<super::pipe::Pipe>),
    PipeWrite(Arc<super::pipe::Pipe>),
    Device(DeviceKind),
}

pub struct FileDescription {
    pub object: FileObject,
    pub offset: AtomicU64,
    pub flags: OpenFlags,
    pub status_flags: AtomicU32,
}

pub struct FdTable {
    entries: Vec<Option<(Arc<FileDescription>, FdFlags)>>,
}
```

分层语义：

1. `FdTable` slot 是“每个 fd 独有”的壳，带 `FdFlags`（如 `CLOEXEC`）。
2. `FileDescription` 是 open file description，`dup/fork` 后共享（同 offset）。
3. `FileObject` 才是后端对象（文件/管道/设备）。

这和 BSD/POSIX 模型一致，是你系统调用语义正确性的核心。

## 2.4 页缓存状态机：`PageState`

```rust
pub enum PageState {
    Fetching(Vec<Waker>),
    Cached(PhysAddr),
}

pub enum LookupResult {
    Hit(PhysAddr),
    InitiateFetch,
    WaitingOnFetch,
}
```

状态转换：

1. `Absent -> Fetching`：第一个线程抢到发起 I/O。
2. `Fetching -> Cached`：I/O 完成后 `complete` 唤醒等待者。
3. 并发读取同页时，后到线程不会重复 I/O。

## 2.5 vnode 对象

```rust
pub trait Vnode: Send + Sync {
    fn vnode_id(&self) -> VnodeId;
    fn vtype(&self) -> VnodeType;
    fn size(&self) -> u64;
    fn path(&self) -> &str;
    fn set_size(&self, size: u64);
    fn grab_obj(&mut self) -> Arc<RwLock<VmObject>>;
}

pub struct Ext4Vnode {
    pub ino: u32,
    pub vtype: VnodeType,
    pub file_size: AtomicU64,
    pub path: String,
    pub vm_obj: Option<Arc<RwLock<VmObject>>>,
}
```

意义：

1. syscall 层持有 `Arc<dyn Vnode>`，避免直接暴露 ext4 细节。
2. `grab_obj` 将 FS vnode 与 VM 的 file-backed object 绑定。

## 2.6 挂载映射对象：`MountEntry`

```rust
pub struct MountEntry {
    pub source: String,
    pub source_path: String,
    pub target: String,
    pub fstype: String,
    pub flags: usize,
}
```

你当前 mount 不是切 superblock，而是“命名空间路径 -> 后端路径”的最长前缀翻译。

---

## 3. `kernel/src/fs` 逐文件深入说明

## 3.1 `fs/mod.rs`

只做模块组织：

```rust
pub mod ext4;
pub mod lwext4_disk;
pub mod delegate;
pub mod vnode;
pub mod dentry;
pub mod page_cache;
pub mod fd_table;
pub mod path;
pub mod pipe;
pub mod devfs;
pub mod mount;
pub mod symlink;
pub use vnode::Vnode;
```

重点在于它导出的模块边界很清晰：`delegate` 是后端总线，`fd_table/path/mount` 是 VFS 外壳。

## 3.2 `fs/vnode.rs`

### 3.2.1 `Ext4Vnode` 的职责

1. 保存 inode/type/size/path 元信息。
2. `file_size` 用原子值，避免频繁加锁。
3. `vm_obj` 延迟初始化，只有真的做映射或 pager 时才创建。

关键实现：

```rust
fn grab_obj(&mut self) -> Arc<RwLock<VmObject>> {
    if let Some(obj) = &self.vm_obj {
        return Arc::clone(obj)
    }
    let new_obj = VmObject::new_file(self);
    self.vm_obj = Some(Arc::clone(&new_obj));
    new_obj
}
```

这里是 FS 和 VM 的接缝点。

## 3.3 `fs/dentry.rs`

全局目录项缓存：

```rust
type DentryKey = (VnodeId, String);
static DENTRY_CACHE: IrqSafeSpinLock<Option<BTreeMap<DentryKey, Arc<dyn Vnode>>>> = ...;
```

操作接口很直接：`lookup/insert/invalidate`。

语义边界：

1. 无 LRU/淘汰策略，长时间运行可能无限增长。
2. 目前仅在 `unlinkat` 显式失效一个条目，rename/link 等操作失效策略仍偏弱。

## 3.4 `fs/path.rs`

路径解析流程：

```rust
for component in path.split('/') {
    if let Some(cached) = dentry::lookup(parent_id, component) {
        current = cached;
        continue;
    }
    let result = delegate::fs_lookup(parent_ino, &built_path).await;
    // miss 后用 full path 做 lookup，再构造 vnode 并回填 dentry
}
```

重要细节：

1. `built_path` 是逐步拼接的绝对路径。
2. delegate `Lookup` 现在把参数当 full path 使用，`parent_ino` 实际未用。
3. root inode 固定 2（ext4 约定）。

## 3.5 `fs/mount.rs`

当前 mount 命名空间是“路径映射表”，不是完整 VFS mount tree。

关键逻辑：

```rust
fn best_match(table: &MountTable, path: &str) -> Option<&MountEntry> {
    table.entries.iter()
        .filter(|m| is_target_prefix(path, &m.target))
        .max_by_key(|m| m.target.len())
}

pub fn resolve_to_source(path: &str) -> String {
    let normalized = normalize_absolute_path(path);
    if let Some(m) = best_match(&table, &normalized) {
        let suffix = mount_suffix(&normalized, &m.target);
        return join_mapped_path(&m.source_path, suffix);
    }
    normalized
}
```

语义：

1. 最长前缀匹配，支持挂载点嵌套。
2. `same_mount_domain` 用于禁止 rename/link 跨挂载域。

## 3.6 `fs/page_cache.rs`

这是一个“页状态协调器”，核心不是数据副本，而是并发去重。

关键函数：

```rust
pub fn lookup(vnode_id: VnodeId, page_offset: u64, waker: &Waker) -> LookupResult {
    match map.get_mut(&(vnode_id, page_offset)) {
        Some(PageState::Cached(pa)) => LookupResult::Hit(*pa),
        Some(PageState::Fetching(waiters)) => {
            waiters.push(waker.clone());
            LookupResult::WaitingOnFetch
        }
        None => {
            map.insert((vnode_id, page_offset), PageState::Fetching(Vec::new()));
            LookupResult::InitiateFetch
        }
    }
}
```

再由 `complete` 完成 `Fetching -> Cached` 和批量唤醒。

## 3.7 `fs/fd_table.rs`

这里必须按“语义”来理解，而不是 API 名字。

### 3.7.1 `dup/fork` 为何共享 offset

因为 slot 里放的是 `Arc<FileDescription>`：

```rust
pub fn fork(&self) -> Self {
    Self { entries: self.entries.clone() }
}
```

`clone()` 只是克隆 Arc，不复制 description 内容，所以 parent/child 会共享 `offset` 和 `status_flags`，符合 POSIX open-file-description 语义。

### 3.7.2 `CLOEXEC` 生命周期

`CLOEXEC` 在 slot flags，而不是 description status flags。

```rust
pub fn strip_cloexec(&mut self) {
    for slot in self.entries.iter_mut() {
        if let Some((_, flags)) = slot {
            if flags.contains(FdFlags::CLOEXEC) {
                *slot = None;
            }
        }
    }
}
```

exec 成功后会调用它，关闭标记了 `CLOEXEC` 的 fd。

## 3.8 `fs/pipe.rs`

固定 4KB ring buffer：

```rust
struct PipeBuffer {
    data: [u8; PIPE_BUF],
    head: usize,
    tail: usize,
    len: usize,
    reader_waker: Option<Waker>,
    writer_waker: Option<Waker>,
}
```

语义点：

1. `read`: 空且 writer 关闭 -> EOF(0)；空且 writer 活着 -> `EAGAIN`。
2. `write`: reader 已关 -> `EPIPE`。
3. `PIPE_BUF` 原子写：写入长度 `<= PIPE_BUF` 时，空间不足直接 `EAGAIN`（不做部分写）。

## 3.9 `fs/devfs.rs`

`/dev` 走静态映射，不经 dentry/path 解析：

```rust
match name {
    "null" => DeviceKind::Null,
    "zero" => DeviceKind::Zero,
    "console" | "tty" => { ... }
    _ => return Err(Errno::Enoent),
}
```

输出是 `FileDescription::new(FileObject::Device(...), flags)`。

## 3.10 `fs/symlink.rs`

内存符号链接表（兼容层）：

```rust
struct SymlinkTable {
    links: Vec<(String, String)>, // link -> target
}
```

它有 `MAX_FOLLOW=8` 的展开上限。

现状上，主路径里的 `sys_symlinkat/readlinkat` 已经走 ext4 原语，这个模块更像历史过渡/备用实现。

## 3.11 `fs/lwext4_disk.rs`

这是 lwext4 和 virtio-blk 的关键适配层。

读写策略：

1. 对齐到 512B 且长度足够：直接整扇区读写。
2. 非对齐：读扇区到临时缓冲，局部改写，再写回（RMW）。

关键代码：

```rust
if self.offset == 0 && buf.len() >= BLOCK_SIZE {
    // fast path: full sector
} else {
    // read-modify-write path for unaligned IO
}
```

`seek` 支持 `SEEK_SET/SEEK_CUR/SEEK_END`。

## 3.12 `fs/ext4.rs`

### 3.12.1 并发约束的核心：`DelegateToken`

```rust
pub struct DelegateToken(());
impl DelegateToken {
    pub(crate) unsafe fn new() -> Self { Self(()) }
}

pub fn open(_tok: &mut DelegateToken, path: &str, open_flags: u32) -> Result<Ext4File, i32>
```

所有 I/O API 都要求 `&mut DelegateToken`，保证只有 delegate task 能调用 lwext4。

### 3.12.2 stat / dir 走原生 C API

```rust
bindings::ext4_raw_inode_fill(...)
bindings::ext4_dir_open(...)
bindings::ext4_dir_entry_next(...)
```

你这里不是“打开文件再猜类型”，而是直接查 inode/direntry，语义和效率都更干净。

## 3.13 `fs/delegate.rs`

这是整个 FS 的执行中枢。

### 3.13.1 请求队列

```rust
static REQUEST_QUEUE: IrqSafeSpinLock<VecDeque<FsRequest>> = ...;
static REQUEST_COUNT: AtomicUsize = AtomicUsize::new(0);
static DELEGATE_WAKER: IrqSafeSpinLock<Option<Waker>> = ...;

fn send_request(req: FsRequest) {
    REQUEST_QUEUE.lock().push_back(req);
    REQUEST_COUNT.fetch_add(1, Ordering::Release);
    if let Some(w) = DELEGATE_WAKER.lock().take() { w.wake(); }
}
```

注意：虽然定义了 `CHANNEL_CAPACITY=256`，当前 `send_request` 没有硬限制检查。

### 3.13.2 单任务消费

```rust
async fn delegate_task() {
    crate::fs::ext4::mount()?;
    let mut tok = unsafe { crate::fs::ext4::DelegateToken::new() };
    let mut open_files: [Option<SendExt4File>; MAX_OPEN_FILES] = [None; MAX_OPEN_FILES];
    loop {
        let req = DelegateRecvFuture.await;
        match req { ... }
        crate::executor::yield_now().await;
    }
}
```

关键语义：

1. `MAX_OPEN_FILES=64` 是 delegate 内部 handle 表上限。
2. 每个请求都在这个循环串行执行，天然规避 lwext4 并发问题。
3. `map_backend_path` 每次都会先过 `mount::resolve_to_source`。

### 3.13.3 当前实现细节（容易踩坑）

1. `Lookup` 分支里注释明确“name is already full path”，`parent_ino` 被忽略。
2. `Stat` 分支直接 `ENOSYS`，调用方基本用 `lookup` 替代。
3. `ReadDir` 每次批量最多 `READDIR_BATCH=32`。

---

## 4. syscall 层如何把语义落到 FS 模块

下面按用户态最常用路径展开。

## 4.1 `openat` 路径

入口：`sys_openat_async`。

关键逻辑：

```rust
let raw_path = copyinstr(task, pathname_ptr, 256).await?;
let path_str = absolutize_path(task, dirfd, &raw_path)?;
open(&task.fd_table, &path_str, open_flags, delegate_flags, fd_flags).await
```

`open(...)` 内部：

1. `/dev/*` 直接走 `devfs::open_device`。
2. 普通路径先 `path::resolve`，判断存在性。
3. `O_CREAT` 且不存在：先 `delegate::fs_open_flags(...O_CREAT...)` 创建，再 resolve。
4. `O_DIRECTORY` 检查 vnode 类型。
5. `O_TRUNC` 触发 `delegate::fs_truncate`，并 `page_cache::invalidate_all`。
6. 最终创建 `FileDescription(FileObject::Vnode)` 插入 `FdTable`。

## 4.2 `read` 路径

入口：`sys_read_async`，先根据 `FileObject` 分流：

1. `Device`: null/zero/console 各自语义。
2. `PipeRead`: `PipeReadFuture` + waker。
3. `Vnode`: 页粒度读取。

vnode 分支核心：

```rust
let pa = page_cache_fetch_by_id(id, &path, page_offset * PAGE_SIZE as u64).await?;
let kern = (pa.as_usize() + offset_in_page) as *mut u8;
uiomove(kern, user, chunk, UioDir::CopyOut)
```

重要现状：`page_cache_fetch_by_id` 当前实现是“直接分配 frame + fs_read_page”，没有调用 `page_cache::lookup/complete`，即没有走上面 `page_cache.rs` 的去重状态机。这是当前读路径与缓存模块之间的一个实现缺口。

## 4.3 `write` 路径

入口：`sys_write_async`，同样按对象分流。

vnode 分支：

```rust
if append {
    if let Ok((_, _, file_size)) = delegate::fs_lookup(0, &path).await {
        offset = file_size;
    }
}
match delegate::fs_write_at(&path, offset, &kbuf).await {
    Ok(n) => {
        desc.offset.store(offset + n as u64, Ordering::Relaxed);
        page_cache::invalidate_range(v.vnode_id(), offset, n);
        v.set_size(max(old_size, offset + n as u64));
    }
}
```

这条链路体现了“写后失效缓存 + 推进文件大小”的一致性处理。

## 4.4 `fcntl/dup/close/lseek` 语义

`sys_fcntl` 直接操作 `FdTable`：

1. `F_DUPFD/F_DUPFD_CLOEXEC` -> `insert_from`。
2. `F_GETFD/F_SETFD` -> slot 的 `FdFlags`。
3. `F_GETFL/F_SETFL` -> description 的 `status_flags`。

`sys_lseek` 对 pipe/device 返回 `ESPIPE`，仅 vnode 可 seek。

## 4.5 目录和命名操作

1. `sys_mkdirat_async`: 先 lookup 检查存在，再 `delegate::fs_mkdir`。
2. `sys_unlinkat_async`: 先 lookup 判类型，再 `delegate::fs_unlink`，随后 `dentry::invalidate`。
3. `sys_symlinkat_async`: 解析父目录，目标相对路径会转绝对，再 `delegate::fs_symlink`。
4. `sys_linkat_async/sys_renameat2_async`: 先 `same_mount_domain` 防跨挂载。
5. `sys_getdents64_async`: 调 `delegate::fs_readdir(path, start_idx)`，把 `DirEntryRaw` 打包成 Linux `dirent64`。

## 4.6 挂载与同步

1. `sys_mount_async`:
   - 拿到 `source/target/fstype`
   - 解析 target 为绝对路径并确认是目录
   - `mount::register_mount`
2. `sys_umount2_async`: 参数校验后 `mount::unregister_mount`
3. `sys_fsync_async/sys_sync_async`: 转发到 `delegate::fs_cache_flush(path)`

## 4.7 stat 相关

1. `sys_fstat`: 基于 `FileObject` 在内核组装 `LinuxStat`。
2. `sys_fstatat_async`: 用 `delegate::fs_lookup(0, path)` 拿 `(ino, ftype, size)` 再填 `LinuxStat`。
3. `delegate::FsRequest::Stat` 本身目前是 `ENOSYS`，所以 stat 语义主要靠 lookup 路径撑起来。

---

## 5. 与 `proc` / `vm` 的耦合点

## 5.1 fork/exec 与 fd 语义

`fork.rs`：

```rust
let parent_fds = parent.fd_table.lock();
*child.fd_table.lock() = parent_fds.fork();
*child.cwd.lock() = parent.cwd.lock().clone();
```

`exec.rs`：

```rust
task.fd_table.lock().strip_cloexec();
```

因此：

1. fork 后父子共享 open file description（共享 offset/status）。
2. exec 成功后关闭 `CLOEXEC` fd。

## 5.2 VM 文件页回源

`vm_object.rs` 的 `VnodePager`：

```rust
crate::fs::delegate::fs_read_page(&path, offset as u64, pa.as_usize()).await
crate::fs::delegate::fs_write_at(&path, offset as u64, data).await
```

`VmObject::new_file(vnode)` 会构建 `VnodePager { vnode_id, path, base_offset, valid_bytes }`。

这条链路说明 exec/mmap 的文件页不经过 syscall 层，而是 VM 直接走 delegate。

---

## 6. 当前实现的关键约束与风险点（按重要性）

1. 后端吞吐上限受单 delegate 串行限制。
2. delegate 请求路径和很多 API 的路径缓冲固定 256 字节。
3. delegate 内打开文件表 `MAX_OPEN_FILES=64`，超出返回 `EMFILE`。
4. `FsRequest::Stat` 仍是 `ENOSYS`，stat 能力主要依赖 `Lookup`。
5. `dentry/page_cache` 都没有淘汰策略，长运行会增长。
6. `send_request` 未执行 `CHANNEL_CAPACITY` 限流检查。
7. `sys_read_async` 当前页获取实现没有接入 `page_cache.rs` 的并发去重状态机。
8. mount 是路径翻译模型，不是完整多 superblock 的 VFS 挂载体系。

---

## 7. 你这套设计的“稳定核心”是什么

如果你要“不要破坏原本设计”，建议把下面这些视为红线：

1. “所有 ext4 I/O 必须经过 delegate 串行化”这个约束不要打破。
2. `FdTable -> FileDescription -> FileObject` 三层模型不要简化成单层。
3. `CLOEXEC` 在 fd slot（不是 description）这一点不要改位。
4. mount 的语义是“路径域翻译”，不是“真实切换后端实例”，接口语义要一致。
5. VM file pager 和 syscall file I/O 最终都应汇聚到同一后端（delegate）。

---

## 8. 快速定位索引

1. 请求协议与串行执行：`kernel/src/fs/delegate.rs`
2. lwext4 封装与 token 约束：`kernel/src/fs/ext4.rs`
3. 块设备适配：`kernel/src/fs/lwext4_disk.rs`
4. fd 三层模型：`kernel/src/fs/fd_table.rs`
5. 路径解析与 dentry：`kernel/src/fs/path.rs`, `kernel/src/fs/dentry.rs`
6. mount 前缀映射：`kernel/src/fs/mount.rs`
7. pipe/dev 语义：`kernel/src/fs/pipe.rs`, `kernel/src/fs/devfs.rs`
8. syscall 总入口：`kernel/src/syscall/fs.rs`
9. VM 文件页回源：`kernel/src/mm/vm/vm_object.rs`

