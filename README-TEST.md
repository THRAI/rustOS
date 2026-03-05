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

## 方法二：basic 自动评分（推荐）

判分脚本位于 `judge/basic/`，`judge/batch_judge.sh` 会自动调用它们。

```bash
# 默认：make oscomp-basic + 评分
./judge/batch_judge.sh

# 跑 basic-musl + basic-glibc 并评分
./judge/batch_judge.sh --target oscomp-basic-all

# 使用已有日志评分（不重新运行 QEMU）
./judge/batch_judge.sh --log /tmp/oscomp.log
```

说明：
1. 脚本会先运行 `make oscomp-*`，结束后再输出评分。
2. 如果内核 panic 或 QEMU 不退出，可用 `--timeout` 缩短等待时间，例如：`--timeout 60`。
3. 评分输出形如 `[basic-musl] checks: 89/102`，表示通过断言数/总断言数。

## 方法三：固定基线并做对照表

建议每次文件系统改动后固定跑一遍：

```bash
./judge/batch_judge.sh --target oscomp-basic-all | tee /tmp/oscomp-basic-all.log
./judge/basic/make_baseline_row.py --log /tmp/oscomp-basic-all.log --repo .
```

第二条命令会输出一行 Markdown，可直接粘到你的对照表中。

推荐基线表头：

```markdown
| Date | Commit | basic-musl | basic-glibc | Note |
|---|---|---|---|---|
```

## 结果判定说明

1. `oscomp-basic` 只会打印 `basic-musl` 组。
2. `oscomp-basic-all` 会依次打印：
   - `#### OS COMP TEST GROUP START basic-musl ####`
   - `#### OS COMP TEST GROUP START basic-glibc ####`
3. 在 `autotest` 模式下，测试子进程结束后 `initproc` 会触发 `shutdown`，QEMU 应自动退出。

## 已知日志噪声

1. `[signal] ERROR: check_pending ... SIGCHLD(17)` 当前是日志级别问题，不代表 basic 用例失败。
2. `TODO: file-backed vma concepts to VmObject layer` 为调试输出，不影响 basic 通过判定。
