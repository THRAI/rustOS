# `syscall/memory.rs` 重构与规范化修改计划

## 1. 文档目标

本文档给出一份**完整、可执行、按步骤推进**的修改计划，用于把当前 `kernel/src/syscall/memory.rs` 中与虚拟内存相关的系统调用实现，重构为更符合当前内核 VM 分层设计的实现。

目标不是一次性“全部推倒重写”，而是：

1. 先修正明显错误和重复逻辑；
2. 再把越层操作下沉到 `VmMap` / `VmObject`；
3. 最后把 `syscall` 层收敛成“参数校验 + 调用 VM API + errno 转换”。

---

## 2. 当前问题总览

重点文件：

- `kernel/src/syscall/memory.rs`
- `kernel/src/mm/vm/map/mod.rs`
- `kernel/src/mm/vm/map/entry.rs`
- `kernel/src/mm/vm/vm_object.rs`
- `kernel/src/mm/vm/fault.rs`
- `kernel/src/mm/vm/fault_async.rs`
- `kernel/src/mm/pmap/mod.rs`

### 2.1 当前最主要的问题

#### 问题 A：`syscall` 层知道太多 VM 细节
当前 `syscall/memory.rs` 直接做了很多本应由 VM 层负责的事：

- 手动拆 PTE
- 直接操作 `VmMapEntry` 边界
- 直接决定对象创建策略
- 直接决定何时 drop 被移除的对象

这会导致：

- 分层不清晰
- 后续并发/一致性难维护
- `VmMap` 的统计字段和增强信息容易失真

#### 问题 B：`VmMap` 和 `Pmap` 的职责边界混乱
现在存在大量重复：

- `VmMap::remove_range()` 内部会调用 `pmap_remove()`
- `sys_mmap(MAP_FIXED)` / `sys_munmap()` 外面又再调一次 `pmap_remove()`
- `VmMap::protect_range()` 内部调 `pmap_protect()`
- `sys_mprotect()` 外面又调一次 `pmap_protect()`

这说明“谁负责同步硬件页表”还没有统一。

#### 问题 C：VMA 元数据更新不完整
目前最危险的点是：

- `mprotect` 没有正确更新 `VmMapEntry.protection`
- `brk` 直接 `set_bounds()`，没有统一维护 `VmMap.size`、`timestamp` 等状态

#### 问题 D：`sys_mmap()` 的语义不完整
当前 `sys_mmap()` 虽然接口看起来像通用 `mmap`，但实际上：

- `fd` 没用
- `offset` 没用
- `map_anon` 没真正参与逻辑分流
- 最终永远是匿名 `VmObject`

所以它实际上只是“匿名映射版本的 `mmap`”。

#### 问题 E：`VmObject.size` 的单位使用不一致
目前代码里有些地方把 `size` 当字节，有些地方当页数，尤其 `sys_brk()` 里比较明显。这会导致对象大小、fault 偏移、truncate 逻辑存在潜在错误。

---

## 3. 本次重构的总原则

### 原则 1：`syscall` 层只做三件事

`syscall/memory.rs` 最终只负责：

1. 参数对齐与合法性检查；
2. 从 fd / flag / prot 翻译成 VM 层参数；
3. 调用 `VmMap` / `VmObject` 提供的正式接口，并返回 errno。

### 原则 2：VMA 修改统一由 `VmMap` 完成

凡是涉及：

- 插入 VMA
- 删除 VMA
- 拆分 VMA
- 合并 VMA
- 改变 VMA 边界
- 改变 VMA 权限

都应该由 `VmMap` 提供 API，而不是由 `syscall` 层直接扒开 `VmMapEntry` 改字段。

### 原则 3：对象内容统一由 `VmObject` 管理

凡是涉及：

- 匿名对象创建
- 文件对象创建
- shadow 对象
- 对象大小变化
- truncate 页
- fault 分配页 / COW 页

都应该由 `VmObject` 自己负责语义闭环。

### 原则 4：页表同步要有唯一责任方

推荐采用以下规则：

- **`VmMap` 负责逻辑映射，同时在 map-level 操作中同步 `Pmap`**。
- `syscall` 层不再直接调用 `pmap_remove()` / `pmap_protect()`。

这样能和你当前 `VmMap::remove_range()`、`VmMap::clear()` 的方向保持一致。

### 原则 5：对象大小统一成“字节数”

建议把 `VmObject.size` 统一定义为：

- **字节数**

理由：

- 文件大小天然是字节
- `mmap/brk` 参数是字节
- `exec` segment 的 `memsz/filesz` 是字节
- 页数可以临时通过 `div_ceil(PAGE_SIZE)` 推导

---

## 4. 重构阶段划分

整个修改分成 6 个阶段。每个阶段都应保证：

- 能单独提交
- 能单独编译
- 有明确验收点

---

# 阶段一：建立规则边界，先修明显错误

## 4.1 目标

先把当前最危险的重复逻辑和明显语义错误收掉，不做大规模 API 迁移。

## 4.2 要修改的点

### 步骤 1：修正 `sys_mmap()` 的语义边界
文件：`kernel/src/syscall/memory.rs`

#### 当前问题
当前 `sys_mmap()` 的行为与其接口不一致：

- 看起来支持文件映射
- 实际只创建匿名对象

#### 修改方案
短期推荐先做“显式降级”版本：

- 如果 `flags` 不含 `MAP_ANONYMOUS`：
  - 直接返回 `-ENOSYS` 或 `-EINVAL`
- 如果 `fd` / `offset` 与匿名映射语义冲突：
  - 返回错误
- 在注释里明确：当前仅支持匿名映射

#### 这样做的原因
先保证“实现语义和接口语义一致”，再做文件映射支持。

#### 验收标准
- 非匿名 `mmap` 不再静默创建匿名对象
- 匿名 `mmap` 现有路径不退化

---

### 步骤 2：删掉 syscall 层重复的 `pmap_remove()`
文件：`kernel/src/syscall/memory.rs`

#### 涉及位置
- `sys_mmap()` 的 `MAP_FIXED` 路径
- `sys_munmap()`

#### 当前问题
`vm.remove_range()` 已经做了 `pmap_remove()`，syscall 再调一次是重复的。

#### 修改方案
删除：

- `sys_mmap(MAP_FIXED)` 里多余的 `pmap_remove()`
- `sys_munmap()` 里多余的 `pmap_remove()`

#### 验收标准
- 行为不变
- 页表拆除只在一处发生
- `remove_range()` 成为唯一 map-range teardown 入口

---

### 步骤 3：删掉 syscall 层重复的 `pmap_protect()`
文件：`kernel/src/syscall/memory.rs`

#### 当前问题
`vm.protect_range()` 已经调用了 `pmap_protect()`，syscall 又调了一次。

#### 修改方案
先删除 syscall 层那一层重复调用。

#### 验收标准
- `sys_mprotect()` 只通过 `VmMap` 路径修改映射
- 当前功能不倒退

---

### 步骤 4：清理无用 import 和旧设计残留
文件：`kernel/src/syscall/memory.rs`

#### 重点
删除不用的：

- `VmAreaType`
- 其它历史版本残留 import

#### 验收标准
- 文件告别“旧接口拼接感”
- import 只保留当前设计所需内容

---

## 4.3 阶段一验收

完成后应该满足：

1. `syscall/memory.rs` 不再重复直接操作 `Pmap`
2. `sys_mmap()` 的匿名/非匿名边界明确
3. 文件整体更容易继续重构

---

# 阶段二：统一 `VmObject.size` 语义

## 5.1 目标

彻底统一 `VmObject.size` 的单位，消除后续所有对象长度相关 bug 的根源。

## 5.2 要修改的点

### 步骤 1：确认统一单位为“字节数”
文件：`kernel/src/mm/vm/vm_object.rs`

#### 修改内容
- 明确 `VmObject.size` 注释：字节数
- 检查所有构造函数参数注释
- 检查所有调用点

### 步骤 2：修正 `new_anon()` 的调用者
重点文件：

- `kernel/src/syscall/memory.rs`
- `kernel/src/proc/exec.rs`
- `kernel/src/proc/fork.rs`
- 其它 `new_anon()` 使用点

#### 当前问题
大量调用者把“页数”传给了可能按“字节数”理解的对象大小字段。

#### 修改方案
统一为：

- `VmObject::new_anon(size_bytes)`

例如：

- `aligned_len` 直接传字节长度
- `grow_len` 直接传字节长度
- `USER_STACK_SIZE` 直接传字节长度

### 步骤 3：修正 `set_size()` 的调用者
重点文件：

- `kernel/src/syscall/memory.rs`
- 其它直接调用 `set_size()` 的地方

#### 修改方案
统一传字节数，而不是页数。

### 步骤 4：检查 `truncate_pages()` 调用点
因为 `truncate_pages()` 用的是页索引，所以要明确：

- `size` 是字节
- `truncate_pages()` 仍然接收页索引
- 调用前由调用者自行换算 `bytes -> VirtPageNum`

---

## 5.3 阶段二验收

完成后应满足：

- `VmObject.size` 只有一种解释：字节数
- `new_anon()` / `set_size()` / `size()` 语义一致
- `sys_brk()` 不再混用页数和字节数

---

# 阶段三：把 `mprotect` 正式下沉到 `VmMap`

## 6.1 目标

让 `mprotect` 真正成为 `VmMap` 的职责，而不是 syscall 层或单纯硬件权限修改。

## 6.2 为什么这一步很重要

当前 `mprotect` 最大的问题是：

- 只改了 PTE
- 没正确改 `VmMapEntry.protection`
- 也不支持部分区间 split

这会直接导致 fault 路径和 VMA 元数据失配。

## 6.3 新接口建议
文件：`kernel/src/mm/vm/map/mod.rs`

建议新增正式接口：

- `pub fn mprotect_range(&mut self, start: VirtAddr, end: VirtAddr, perm: MapPerm) -> Result<(), VmError>`

### 该接口内部职责
1. 参数合法性检查；
2. 找到所有覆盖 `[start, end)` 的 `VmMapEntry`；
3. 对部分覆盖区间执行 split；
4. 更新目标 entry 的 `protection` / `max_protection`（视策略而定）；
5. 更新 `timestamp`；
6. 最后统一 `pmap_protect()`。

## 6.4 具体拆分策略

### 情况 1：完全覆盖一个 entry
直接改其 `protection`

### 情况 2：修改 entry 左半段
拆成：

- `[start0, end)` 新权限
- `[end, old_end)` 旧权限

### 情况 3：修改 entry 右半段
拆成：

- `[old_start, start)` 旧权限
- `[start, old_end)` 新权限

### 情况 4：修改中间一段
拆成三段：

- 左段旧权限
- 中段新权限
- 右段旧权限

## 6.5 syscall 层怎么改
文件：`kernel/src/syscall/memory.rs`

`sys_mprotect()` 最终只负责：

1. 对齐地址；
2. 转换 `prot_bits -> MapPerm`；
3. 调 `vm.mprotect_range(...)`；
4. 转成 errno。

---

## 6.6 阶段三验收

完成后应满足：

- `mprotect` 修改的是 `VmMapEntry` 的正式元数据
- 页表权限和 VMA 权限一致
- fault handler 能看到更新后的权限

---

# 阶段四：把 `brk` 的边界修改收敛到 `VmMap`

## 7.1 目标

禁止 syscall 层直接 `set_bounds()`，所有 VMA 边界变化都由 `VmMap` 自己维护。

## 7.2 当前问题

现在 `sys_brk()` 在做：

- `vma.set_bounds(...)`
- `object.set_size(...)`
- `truncate_pages(...)`
- `pmap_remove(...)`

这等于 syscall 越层直接操纵：

- VMA 拓扑
- 对象大小
- 页回收
- 页表拆除

而 `VmMap` 的 `size`、`timestamp` 并没有被统一维护。

## 7.3 新接口建议
文件：`kernel/src/mm/vm/map/mod.rs`

建议新增：

- `pub fn grow_heap_to(&mut self, old_brk: usize, new_brk: usize) -> Result<(), VmError>`
- `pub fn shrink_heap_to(&mut self, old_brk: usize, new_brk: usize) -> Result<alloc::vec::Vec<VmMapEntry>, VmError>`

或者更抽象一点：

- `resize_heap_vma(...)`

## 7.4 `VmMap` 内部职责

### grow path
1. 找当前 heap entry；
2. 判断是否可原地扩展；
3. 更新 VMA 边界；
4. 更新 `VmObject.size`；
5. 更新 `VmMap.size`；
6. 更新 `timestamp`。

### shrink path
1. 找当前 heap entry；
2. 计算新边界；
3. 更新对象大小；
4. truncate 超出页；
5. 拆除对应 `pmap` 范围；
6. 如有必要完全移除 heap VMA；
7. 更新 `VmMap.size` 和 `timestamp`。

## 7.5 syscall 层怎么改
文件：`kernel/src/syscall/memory.rs`

`sys_brk()` 最终应只做：

1. 读当前 `task.brk`；
2. 计算对齐值；
3. 调用 `VmMap` heap API；
4. 成功后更新 `task.brk`；
5. 失败则保留旧 brk。

---

## 7.6 阶段四验收

完成后应满足：

- `sys_brk()` 不再直接 `set_bounds()`
- `VmMap.size` 与实际 VMA 总长度一致
- `timestamp` 在 heap 改动时正确递增
- heap 缩小后的页回收与页表拆除由 VM 层完成

---

# 阶段五：给 `VmMap` 增加正式映射 API，收口 `sys_mmap/munmap`

## 8.1 目标

把当前 syscall 层里的大量 VMA 建模逻辑移到 `VmMap`。

## 8.2 建议新增 API
文件：`kernel/src/mm/vm/map/mod.rs`

建议逐步引入：

- `map_anon(...)`
- `map_file(...)`
- `unmap_range(...)`
- `find_map_space(...)`

推荐先从最容易落地的开始：

### 第一批 API
- `pub fn map_anon(&mut self, base: Option<VirtAddr>, len: usize, perm: MapPerm, fixed: bool) -> Result<VirtAddr, VmError>`
- `pub fn unmap_range(&mut self, start: VirtAddr, end: VirtAddr) -> Result<alloc::vec::Vec<VmMapEntry>, VmError>`

## 8.3 `map_anon()` 内部职责

1. 地址合法性检查；
2. `MAP_FIXED` / hint / topdown 决策；
3. 如 fixed，则先移除冲突区；
4. 创建匿名 `VmObject`；
5. 创建 `VmMapEntry`；
6. 插入 map；
7. 返回基地址。

## 8.4 `unmap_range()` 内部职责

1. 调已有 `remove_range()`；
2. 统一处理 metadata；
3. 返回 removed entries 给调用方或内部 drop。

## 8.5 syscall 层怎么改

### `sys_mmap()`
最终只做：

1. 参数校验；
2. 权限位翻译；
3. flag 检查；
4. 调 `vm.map_anon(...)` 或以后调 `vm.map_file(...)`；
5. 返回地址或 errno。

### `sys_munmap()`
最终只做：

1. 地址对齐；
2. 调 `vm.unmap_range(...)`；
3. 释放 removed entries；
4. 返回 errno。

---

## 8.6 阶段五验收

完成后应满足：

- `sys_mmap/munmap` 变薄
- `VmMap` 成为唯一的地址空间修改入口
- syscall 层不再直接构造过多 VM 细节

---

# 阶段六：补齐文件映射路径

## 9.1 目标

在匿名映射路径收敛完成后，再补文件映射，不把两件事混在一起做。

## 9.2 新接口建议
文件：`kernel/src/mm/vm/map/mod.rs`

新增：

- `pub fn map_file(&mut self, base: Option<VirtAddr>, len: usize, perm: MapPerm, fixed: bool, object: Arc<spin::RwLock<VmObject>>, file_offset: u64) -> Result<VirtAddr, VmError>`

## 9.3 syscall 层逻辑
文件：`kernel/src/syscall/memory.rs`

### `sys_mmap()` 文件映射路径需要做的事
1. 从 `fd` 找到对应文件对象；
2. 验证是否可映射；
3. 获取 `vnode` 或 page-cache 对象；
4. 构造文件 `VmObject`；
5. 调 `vm.map_file(...)`。

## 9.4 配合 fault 路径验证
因为你已经有：

- `sync_fault_handler()`
- `resolve_user_fault()`
- `fault_in_page_async()`

所以一旦 `BackingStore::Object` 后面挂的是文件对象，缺页时就会沿着 pager 路径取页。

这说明当前内核其实已经具备文件映射的基础，只是 syscall 入口没有补齐。

---

## 9.5 阶段六验收

完成后应满足：

- `mmap(file)` 不再退化成匿名映射
- `fd` / `offset` 真正生效
- 文件页经 pager 进入 `VmObject`

---

## 10. 每个阶段建议的提交策略

建议每一阶段单独一个 commit，顺序如下：

1. `memory: remove duplicate pmap teardown/protect from syscalls`
2. `vm_object: unify object size semantics to bytes`
3. `vm_map: implement metadata-correct mprotect_range`
4. `vm_map: move brk heap resizing logic into vm layer`
5. `vm_map: add map_anon/unmap_range and thin syscall wrappers`
6. `memory: add file-backed mmap path`

这样便于：

- 回滚
- bisect
- 对比行为变化
- 单阶段调试

---

## 11. 测试与验证计划

每个阶段都至少做以下验证。

### 11.1 匿名映射测试
- `mmap` 成功返回地址
- 首次访问触发缺页并成功映射
- `munmap` 后再次访问 fault

### 11.2 `MAP_FIXED` 测试
- 覆盖原区间时旧 VMA 被移除
- 没有重复 teardown 异常

### 11.3 `mprotect` 测试
- 改只读后写入 fault
- 改可执行后执行 fault 消失
- 部分区间 `mprotect` 后 VMA 正确 split

### 11.4 `brk` 测试
- 堆扩展后地址连续
- 堆缩小后页表拆除
- 再访问缩小后的区域应 fault

### 11.5 文件映射测试
- `mmap(file)` 能返回映射地址
- 首次访问页面能从 pager 读入
- offset 映射正确

### 11.6 元数据一致性测试
重点检查：

- `VmMap.size`
- `VmMap.nentries`
- `VmMap.timestamp`
- `VmObject.size`
- `VmObject.resident_count`

---

## 12. 推荐的落地顺序（真正执行时）

如果你现在准备开改，建议严格按下面顺序来：

### 第 1 步
先做阶段一：

- 清理 syscall 里的重复 `pmap_*`
- 明确 `sys_mmap()` 只支持匿名映射

### 第 2 步
做阶段二：

- 统一 `VmObject.size` 单位
- 修正 `new_anon()` / `set_size()` 所有调用点

### 第 3 步
做阶段三：

- 重写 `VmMap::protect_range()`
- 让 `sys_mprotect()` 只剩薄封装

### 第 4 步
做阶段四：

- 把 `brk` 的伸缩逻辑下沉到 `VmMap`

### 第 5 步
做阶段五：

- 引入 `map_anon()` / `unmap_range()`
- 缩薄 `sys_mmap()` / `sys_munmap()`

### 第 6 步
最后做阶段六：

- 完整补齐文件映射

---

## 13. 最终理想状态

当这套重构完成后，职责应当变成：

### `syscall/memory.rs`
只负责：

- 参数检查
- errno
- fd/flag/prot 翻译
- 调 VM API

### `VmMap`
负责：

- VMA 插入、删除、拆分、合并
- heap 扩缩
- 权限修改
- 同步 `Pmap`
- 维护 `size` / `nentries` / `timestamp`

### `VmObject`
负责：

- 对象大小
- 页面集合
- truncate
- fault 分配匿名页
- COW
- pager 对接

### `Pmap`
只负责：

- 建页表
- 拆页表
- 改硬件权限
- 抽取 PTE

---

## 14. 一句话结论

这次修改不要直接“重写 syscall 文件”，而要按以下路线推进：

> **先消除重复页表操作，再统一对象大小语义；随后把 `mprotect` 和 `brk` 的真实内存语义下沉到 `VmMap`；最后再把 `mmap/munmap` 收口成正式 VM API，并补齐文件映射支持。**

这样改，风险最低、收益最大，也最符合你现在这套 `VmMap -> VmMapEntry -> VmObject -> VmPage -> Pmap` 的架构方向。
