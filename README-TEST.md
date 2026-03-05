# 测试说明

## 依赖

```bash
sudo apt install qemu-system-riscv64 e2fsprogs
rustup component add llvm-tools-preview
cargo install cargo-binutils
```

## 方法一：赛题标准评测

```bash
make oscomp            # 全量测试（musl + glibc 全套）
make oscomp-basic      # 仅 basic-musl
make oscomp-basic-all  # basic-musl + basic-glibc
```

自动完成：生成 `scripts/sdcard-rv.img` → 编译内核 → QEMU 启动评测。

> 需要 `sudo`（挂载 ext4 镜像），`testcase/` 目录须存在。

## 结果判定说明

1. `oscomp-basic` 只会打印 `basic-musl` 组。
2. `oscomp-basic-all` 会依次打印：
   - `#### OS COMP TEST GROUP START basic-musl ####`
   - `#### OS COMP TEST GROUP START basic-glibc ####`
3. 在 `autotest` 模式下，测试子进程结束后 `initproc` 会触发 `shutdown`，QEMU 应自动退出。

## 已知日志噪声

1. `[signal] ERROR: check_pending ... SIGCHLD(17)` 当前是日志级别问题，不代表 basic 用例失败。
2. `TODO: file-backed vma concepts to VmObject layer` 为调试输出，不影响 basic 通过判定。
