# rustOS 重构进度报告

## 目标
将 `kernel/src/executor/user_task.rs` (2988行) 重构为模块化架构，参考 Chronix 和 Del0n1x 的设计模式。

## 已完成工作

### ✅ 阶段1：创建syscall层骨架
创建了统一的系统调用层：
- `kernel/src/syscall/mod.rs` - 分发器框架和SyscallId枚举
- `kernel/src/syscall/process.rs` - 进程管理（重导出proc/syscalls.rs）
- `kernel/src/syscall/memory.rs` - 内存管理骨架
- `kernel/src/syscall/fs.rs` - 文件系统骨架
- `kernel/src/syscall/io_async.rs` - 异步I/O骨架
- `kernel/src/syscall/sync.rs` - 同步原语骨架
- `kernel/src/syscall/signal.rs` - 信号处理（重导出proc/syscalls.rs）
- `kernel/src/syscall/misc.rs` - 杂项骨架

### ✅ 阶段2：提取独立模块
从user_task.rs中提取了约550行代码：

1. **`proc/user_copy.rs` (180行)**
   - `fault_in_user_buffer()` - 预触发页面错误
   - `copyinstr()` - 复制用户字符串
   - `copyin_argv()` - 复制参数数组
   - `do_exit()` - 进程退出

2. **`mm/vm/fault_async.rs` (370行)**
   - `resolve_user_fault()` - 页面错误解决
   - `fault_in_page_async()` - 异步页面错误
   - `page_cache_fetch_by_id()` - 页面缓存
   - `PageCacheWaitFuture` - 等待Future
   - `noop_waker()` - 无操作唤醒器

## 进行中工作

### 🔄 阶段3：重构分发器并连接模块
需要完成的任务：

1. **更新user_task.rs的import**
   - 将内部函数调用改为使用新模块
   - `fault_in_user_buffer` → `crate::proc::user_copy::fault_in_user_buffer`
   - `copyinstr` → `crate::proc::user_copy::copyinstr`
   - `copyin_argv` → `crate::proc::user_copy::copyin_argv`
   - `do_exit` → `crate::proc::user_copy::do_exit`
   - `resolve_user_fault` → `crate::mm::vm::fault_async::resolve_user_fault`

2. **提取系统调用实现到syscall模块**
   需要从user_task.rs的dispatch_syscall函数中提取：
   
   - **syscall/memory.rs** (~400行)
     - `sys_mmap()` - 完整实现
     - `sys_munmap()` - 完整实现
     - `sys_mprotect()` - 完整实现
     - `sys_brk()` - 从dispatch_syscall中的BRK case提取
     - `free_removed_frames()` - 辅助函数
   
   - **syscall/fs.rs** (~300行)
     - `sys_lseek()` - 完整实现
     - `sys_fstat()` - 完整实现
     - `sys_getcwd()` - 完整实现
     - `sys_fcntl()` - 完整实现
     - `sys_pipe2()` - 完整实现
     - `LinuxStat` 结构体
     - `absolutize_path()` - 辅助函数
     - `normalize_absolute_path()` - 辅助函数
   
   - **syscall/io_async.rs** (~800行)
     - `sys_read_async()` - 完整实现
     - `sys_write_async()` - 完整实现
     - `sys_readv_async()` - 完整实现
     - `sys_writev_async()` - 完整实现
     - `sys_openat_async()` - 完整实现
     - `sys_chdir_async()` - 完整实现
     - `sys_fstatat_async()` - 完整实现
     - `sys_ioctl_async()` - 完整实现
     - `sys_ppoll_async()` - 完整实现
     - `PipeReadFuture` - Future实现
     - `PipeWriteFuture` - Future实现
     - `ConsoleReadFuture` - Future实现
   
   - **syscall/sync.rs** (~300行)
     - `sys_clock_gettime()` - 完整实现
     - `sys_nanosleep_async()` - 完整实现
     - `sys_futex_async()` - 完整实现
     - `read_rdtime()` - 辅助函数
     - 常量定义（TIMER_FREQ等）
   
   - **syscall/misc.rs** (~200行)
     - `sys_uname()` - 完整实现

3. **重构dispatch_syscall为syscall::syscall()**
   - 将大match语句改为调用各子模块的函数
   - 保持异步语义不变
   - 处理EINTR和SA_RESTART逻辑

4. **更新user_trap_handler**
   - 将dispatch_syscall调用改为syscall::syscall()

## 待完成工作

### 📋 阶段4：精简user_task.rs
目标：将user_task.rs从2988行精简到~300行

保留内容：
- `run_tasks()` - 主trap循环
- `user_trap_handler()` - trap分发
- `SignalWakeHelper` - 信号唤醒
- `spawn_user_task()` - 用户任务生成
- `PmapWrapper` - 页表激活
- `TrapResult` 枚举
- 常量定义（SCAUSE_*, IRQ_*, EXC_*）

删除内容：
- 所有系统调用实现（已移到syscall/）
- 所有辅助函数（已移到proc/user_copy.rs和mm/vm/fault_async.rs）
- SyscallId枚举（已移到syscall/mod.rs）

### 📋 阶段5：添加user目录
创建用户态程序支持：
```
user/
├── Cargo.toml
├── Makefile
└── src/
    ├── lib.rs          # 用户态库
    ├── syscall.rs      # 系统调用封装（ecall）
    └── bin/
        ├── init.rs     # init进程
        └── shell.rs    # shell程序
```

## 编译状态
✅ 当前所有模块编译通过
⚠️ 有未使用警告（预期的，将在阶段3连接后解决）

## 下一步行动
1. 从user_task.rs中提取sys_mmap/munmap/mprotect到syscall/memory.rs
2. 提取sys_lseek/fstat/getcwd等到syscall/fs.rs
3. 提取sys_read/write等到syscall/io_async.rs
4. 提取sys_clock_gettime/nanosleep/futex到syscall/sync.rs
5. 更新dispatch_syscall调用新模块
6. 测试编译和功能

## 参考架构
- **Chronix**: `os/src/syscall/` 统一系统调用层
- **Del0n1x**: `os/src/syscall/mod.rs` 的 `async fn syscall()` 分发模式
