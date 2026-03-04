# rustOS 文件系统能力评估与补全计划

更新时间：2026-03-04  
范围：`testsuits-for-oskernel` 文件系统相关需求、`test/rustOS` 当前实现、`jiagou` 架构一致性检查

---

## 1. 目标完成度定义（按测评场景）

| 级别 | 目标 | 通过标准 |
|---|---|---|
| L0 | 可启动 + 可执行用户程序 | `/bin/initproc` 可执行，`open/read/execve` 主路径稳定 |
| L1 | `basic` 文件系统相关用例通过 | `run-all.sh` 中 `getcwd/chdir/openat/close/read/write/fstat/getdents/mkdir/unlink/mount/umount/pipe/dup/dup2` 达标 |
| L2 | `busybox_testcode.sh` 主要文件操作通过 | `touch/cat/echo>/>>/rm/mkdir/mv/rmdir/cp/find/ls/stat` 基本可用 |
| L3 | `libc-test/LTP` 文件系统子集通过 | `fstatat/readlinkat/renameat2/faccessat/ftruncate/fsync/...` 语义趋近 Linux |

当前状态判断：约在 **L2- ~ L2**。  
目录能力、普通文件读写、`touch/echo/cat/ls` 主路径已可用，后续主要缺口转为挂载与运维类 syscall。

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
| `symlinkat` | 未实现 | 当前直接 `ENOSYS` |
| `linkat` | 未实现 | 无 syscall 入口 |
| `mount/umount2` | 未实现 | 启动阶段内核内部 mount，用户态 syscall 缺失 |
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
2. 挂载与运维 syscall 缺失：`mount/umount2/linkat/rename/readlink` 尚未接入。
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

结论：架构方向正确，功能已从“目录可用”推进到“基础文件可写”，但语义完整性仍不足以稳定冲击 L2/L3。

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

状态：**未开始**

### 阶段 D：元数据与兼容增强

状态：**未开始**

---

## 6. 下一步应执行的阶段

当前建议：**进入阶段 C（挂载兼容与目录运维能力）**。  
原因：阶段 B 的语义收敛已完成，下一步瓶颈已转移到 `mount/umount/rename/link/readlink` 能力缺失。

阶段 C 起步任务（优先级顺序）：
1. 实现 `mount(40)/umount2(39)` 的最小可用语义（先对齐 `basic`/oscomp 返回行为）。
2. 增加 `rename/renameat2` 最小子集，打通 `mv` 主路径。
3. 增加 `linkat/readlinkat` 最小实现，提升 busybox 与后续 LTP 兼容。
4. 补一轮运维命令回归：`mv/rmdir/cp/find`。

---

## 7. 阶段 C 预告（B 完成后立即进入）

目标：
1. 实现 `mount(40)/umount2(39)` 最小可用语义（满足测试返回与基本行为）。
2. 增加 `rename`（建议 `renameat2` 最小子集）以支撑 `mv`。
3. 增加 `linkat/readlinkat`，提升工具链兼容。

---

## 8. 当前结论

1. 当前文件系统实现已推进到 **L2- ~ L2**，阶段 B 已完成收尾。  
2. 下一步应进入阶段 C，集中补齐挂载与目录运维 syscall。  
3. 当前主要风险已从文件 I/O 语义转移到系统调用覆盖度与运维语义完整性。  
