# 测试说明

## 依赖

```bash
sudo apt install qemu-system-riscv64 e2fsprogs
rustup component add llvm-tools-preview
cargo install cargo-binutils
```

## 方法一：赛题标准评测

```bash
make oscomp        # 全量测试
make oscomp-basic  # 仅 basic 测试
```

自动完成：生成 `scripts/sdcard-rv.img` → 编译内核 → QEMU 启动评测。

> 需要 `sudo`（挂载 ext4 镜像），`testcase/` 目录须存在。
