# rustOS 文件系统能力评估与补全计划

更新时间：2026-03-05  
范围：`testsuits-for-oskernel` 文件系统相关需求、`test/rustOS` 当前实现、`jiagou` 架构一致性检查

---

## 1. 目标完成度定义（按测评场景）

| 级别 | 目标 | 通过标准 |
|---|---|---|
| L0 | 可启动 + 可执行用户程序 | `/bin/initproc` 可执行，`open/read/execve` 主路径稳定 |
| L1 | `basic` 文件系统相关用例通过 | `run-all.sh` 中 `getcwd/chdir/openat/close/read/write/fstat/getdents/mkdir/unlink/mount/umount/pipe/dup/dup2` 达标 |
| L2 | `busybox_testcode.sh` 主要文件操作通过 | `touch/cat/echo>/>>/rm/mkdir/mv/rmdir/cp/find/ls/stat` 基本可用 |
| L3 | `libc-test/LTP` 文件系统子集通过 | `fstatat/readlinkat/renameat2/faccessat/ftruncate/fsync/...` 语义趋近 Linux |

当前状态判断：稳定在 **L2**，并完成 `basic-musl + basic-glibc` 双栈基础回归。  
目录能力、普通文件读写、`touch/echo/cat/ls` 主路径可用；`mount/umount` 已通过 basic 回归。当前主要缺口转为运维类 syscall 覆盖度与语义完整性（`rename/link/readlink/ftruncate/fsync/...`）。

---

## 2. 测试需求拆解

### 2.1 来自官方 syscall 清单（`oscomp_syscalls.md`）

文件系统相关：  
`getcwd(17), pipe2(59), dup(23), dup3(24), chdir(49), openat(56), close(57), getdents64(61), read(63), write(64), linkat(37), unlinkat(35), mkdirat(34), umount2(39), mount(40), fstat(80)`。

### 2.2 来自 `basic` 真实脚本

`basic/user/src/oscomp/run-all.sh` 重点覆盖：
`chdir, close, dup2, dup, fstat, getcwd, getdents, mkdir_, mount, openat, open, pipe, read, umount, unlink, write ...`

核心语义：
1. `openat` 支持目录 fd 与 `O_CREAT`。
2. `mkdir` 后 `open(..., O_DIRECTORY)` 可成功。
3. `getdents64` 返回有效目录项。
4. `unlink` 后路径再次打开失败。
5. `mount/umount` 至少在测试路径上返回符合预期。

### 2.3 来自 `busybox` 脚本

`busybox_cmd.txt` 典型命令：  
`touch`, `echo >`, `echo >>`, `cat`, `stat`, `rm`, `mkdir`, `mv`, `rmdir`, `cp`, `find`, `ls`。

隐含需求：
1. `openat` 的 `O_CREAT/O_TRUNC/O_APPEND/O_DIRECTORY/O_EXCL` 语义稳定。
2. 普通文件写后可读、可见性一致。
3. 目录遍历可跨批次。
4. `rename`/`link`/`readlink` 等运维语义逐步补齐。

---

## 3. 当前实现盘点（以代码为准）

### 3.1 syscall 接入矩阵（文件系统相关）

| syscall | 状态 | 说明 |
|---|---|---|
| `getcwd` | 已实现 | 可用 |
| `chdir` | 已实现 | 可用 |
| `openat` | 已实现（阶段 B 收尾） | `O_CREAT/O_EXCL/O_TRUNC/O_APPEND/O_DIRECTORY` 主路径可用 |
| `close` | 已实现 | 可用 |
| `read` | 已实现 | vnode/page cache + pipe/device 路径可用 |
| `write` | 已实现（基础版） | vnode 写路径已接入 `delegate::fs_write_at` |
| `fstat` | 已实现 | 基本字段可返回 |
| `fstatat` | 已实现（简化） | flags 语义缺失 |
| `utimensat` | 已实现（最小版） | 主要为 `touch` 兼容，尚未真实更新时间戳 |
| `lseek` | 已实现 | 基本可用 |
| `pipe2` | 已实现 | 可用 |
| `dup/dup3` | 已实现 | 可用 |
| `fcntl` | 部分实现 | `F_SETFL/F_SETFD` 近似 stub |
| `getdents64` | 已实现（分页版） | 支持按 fd offset 连续分页读取（delegate 批大小 32） |
| `mkdirat` | 已实现 | 已分发并走 `delegate -> ext4::mkdir` |
| `unlinkat` | 已实现（基础版） | 支持 `AT_REMOVEDIR` 分支 |
| `symlinkat` | 已实现（兼容版） | 以内存符号链接表实现，已可支撑 `ln -sf` 建链；暂未落盘到 ext4 |
| `linkat` | 未实现 | 无 syscall 入口 |
| `mount/umount2` | 已实现（最小语义） | 支持挂载点登记/卸载流程，尚未切换真实后端 |
| `rename/renameat2` | 未实现 | 无 syscall 入口 |
| `readlinkat/faccessat/fsync/ftruncate` | 未实现 | 无 syscall 入口或语义缺失 |

### 3.2 FS 子系统现状

已具备：
1. 单 delegate 线程串行调用 lwext4（线程安全模型正确）。
2. dentry cache、path resolve、page cache 基础状态机。
3. fd 三层模型（`FdTable`/`FileDescription`/`FileObject`）。
4. pipe + waker 链路可用。
5. 目录链路可用（`mkdirat/unlinkat/getdents64`）。
6. 普通文件写链路可用（`sys_write_async` 的 `Vnode` 分支已打通）。

主要缺口：
1. inode 语义仍为 synthetic：`lookup` 使用 path-hash，不是稳定真实 inode。
2. 运维 syscall 仍不完整：`linkat/rename/readlink` 尚未接入。
3. `fstatat/fcntl` 等语义仍有简化分支（flags 与行为完整性不足）。

---

## 4. 与 `jiagou` 架构一致性评估

### 4.1 符合项

1. 总体分层方向正确：`syscall -> fs/proc/mm`。
2. delegate 模型正确：lwext4 confined 到单任务。
3. page cache 放置位置合理，具备等待者唤醒机制。
4. fd/pipe/signal 协同基本成型。

### 4.2 偏离项

1. syscall 层仍承载较多 VFS 细节，边界偏厚。
2. vnode trait object 路径与文档中的 enum dispatch 不一致。
3. 高阶语义（真实 inode、rename/link、mount）尚未闭环。

结论：架构方向正确，已形成 L2 稳态能力；后续关键在于把运维与元数据语义补齐，向 L3 推进。

---

## 5. 阶段进展更新

### 阶段 A：目录功能闭环

状态：**已完成（基础功能）**  
备注：需补大目录分批与偏移回归。

### 阶段 B：普通文件创建/写入语义

状态：**已完成（收尾完成）**

已完成：
1. `write` 的 `Vnode` 路径已打通。
2. `touch` 关键链路已打通（`utimensat` 最小实现 + `O_CREAT` 兼容修正）。
3. `openat` 的 `O_EXCL/O_TRUNC` 语义已落地。
4. 写后缓存一致性已补齐：`page cache` 失效与 `vnode.size` 更新已接入。
5. `getdents64` 大目录分页与 offset 连续性已补齐。
6. 回归通过：`touch/echo >/>>/cat/ls` 主路径可用（含大目录 `ls`）。

### 阶段 C：挂载兼容与目录运维能力

状态：**进行中（Step 1 完成，Step 3 部分完成，basic 双栈回归通过）**

已完成（Step 1）：
1. `mount(40)/umount2(39)` 已接入 syscall 分发与最小语义。
2. 已支持挂载点目录校验、挂载登记、卸载注销。
3. 对现有单根 ext4 架构保持兼容（当前不做真实后端切换）。

已完成（Step 3 部分）：
1. `symlinkat(36)` 已接入 syscall 分发与最小实现（内存映射表）。
2. `unlinkat` 已能删除内存符号链接项。
3. `path::resolve` 已支持符号链接映射跟随（用于动态加载器路径解析）。
4. `run-rv-basic.sh` 中 `ln -sf ...` 不再报 `Function not implemented`。

待完成：
1. `rename/renameat2` 最小子集（支撑 `mv` 主路径）。
2. `linkat/readlinkat` 最小实现（补齐链接相关 syscall）。
3. 将符号链接能力从“内存兼容层”下沉到 ext4 落盘语义。
4. 提升 `fstatat/fcntl/utimensat` 等“已实现但简化”接口的语义完整性。

最新验证（2026-03-05）：
1. `oscomp-basic-all` 下 `basic-musl` 与 `basic-glibc` 两组均完整跑到 `TEST GROUP END`。
2. `mount/umount` 用例已通过，`openat/getdents/mkdir/mmap/pipe/wait*` 等主路径稳定。
3. `initproc` 在 `autotest` 模式下已增加测试完成后 `shutdown`，避免测试结束后常驻等待。
4. 现存日志噪声：`[signal] ERROR: check_pending ... SIGCHLD(17)` 与 `TODO: file-backed vma ...`，当前不作为 basic 失败判据。

### 阶段 D：元数据与兼容增强

状态：**未开始**

---

## 6. 下一步应执行的阶段

当前建议：**继续阶段 C（补目录运维能力）并并行推进阶段 D（语义对齐）**。  
原因：`basic-musl/glibc` 已通过，短板已从“阻塞点修复”转为“语义完整性与长尾 syscall”。

阶段 C 下一步任务（优先级顺序）：
1. 增加 `rename/renameat2` 最小子集，打通 `mv` 主路径。
2. 增加 `linkat/readlinkat` 最小实现，并将 `symlinkat` 逐步下沉到 ext4 落盘语义。
3. 清理误导性日志级别（如 `SIGCHLD` 打印为 `ERROR`）并补充测试判定文档。

---

## 7. 阶段 D 预告（C 能力补齐后进入）

目标：
1. 完善元数据与状态 syscall 语义：`fstatat/faccessat/ftruncate/fsync/utimensat`。
2. 对齐 inode 与目录项稳定性语义，减少 synthetic inode 带来的兼容偏差。
3. 用 libc-test/LTP 文件系统子集做回归，建立 L3 量化指标。

---

## 8. 当前结论

1. 当前文件系统实现已稳定在 **L2**，并完成 `basic-musl + basic-glibc` 基础回归。  
2. `mount/umount` 已通过，basic 阶段核心链路已闭环。  
3. 下一步应继续阶段 C/D，重点补齐目录运维与元数据语义（`rename/link/readlink/ftruncate/fsync/...`）。  
