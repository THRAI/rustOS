use crate::hal_common::VirtAddr;

#[derive(Debug, Clone)]
pub struct MemoryRegion {
    pub vaddr: usize,
    pub memsz: usize,
    pub filesz: usize,
    pub offset: usize,
    pub prot: MapPerm,
}

#[derive(Debug, Default)]
pub struct ExecContext {
    pub load_base: VirtAddr,
    pub mapped_regions: Vec<MemoryRegion>,
    pub brk: VirtAddr,

    pub user_entry: VirtAddr,
    pub interp_entry: Option<VirtAddr>,

    pub inerp_path: Option<PathBuf>,

    pub phdr_vaddr: VirtAddr,
    pub phnum: u16,
    pub phentsize: u16,
}

impl ExecContext {
    pub fn build(
        elf_bytes: &[u8],
        ehdr: &goblin::elf::Header,
        phdrs: &[goblin::elf::ProgramHeader],
        load_base: VirtAddr,
    )  -> Self {
        let mut ctx = Self {
            load_base,
            user_entry: VirtAddr(ehdr.entry) + load_base,
            phnum: ehdr.phnum,
            phentsize: ehdr.phentsize,
            ..Default::default()
        };

      let mut max_vaddr = 0;

        // 遍历 Program Headers，寻找构建执行状态所需的关键节点
        for phdr in phdrs {
            match phdr.p_type {
                // PT_LOAD: 构建内存映射 (Memory Map)
                goblin::elf::program_header::PT_LOAD => {
                    let region = MemoryRegion {
                        vaddr: phdr.p_vaddr + load_base,
                        memsz: phdr.p_memsz as usize,
                        filesz: phdr.p_filesz as usize,
                        offset: phdr.p_offset as usize,
                        prot: phdr.p_flags.into(),
                    };
                    ctx.mapped_regions.push(region);

                    // 追踪 Program Break (数据段最高位置)
                    let segment_end = phdr.p_vaddr + load_base + phdr.p_memsz;
                    if segment_end > max_vaddr {
                        max_vaddr = segment_end;
                    }
                }

                // PT_INTERP: 获取动态链接器/解释器的路径
                goblin::elf::program_header::PT_INTERP => {
                    // 从原始字节中切片出 C 字符串并转换为 PathBuf
                    let start = phdr.p_offset as usize;
                    let end = (phdr.p_offset + phdr.p_filesz) as usize;
                    if let Ok(interp_str) = std::str::from_utf8(&elf_bytes[start..end - 1]) { // -1 忽略 \0
                        ctx.interpreter_path = Some(PathBuf::from(interp_str));
                    }
                }

                // PT_PHDR: 记录 Phdrs 自身的加载地址，用于后续传给 auxv 的 AT_PHDR
                goblin::elf::program_header::PT_PHDR => {
                    ctx.phdr_vaddr = phdr.p_vaddr + load_base;
                }
                _ => {} // 忽略与执行状态非强相关的段 (如 PT_NOTE, PT_GNU_STACK)
            }
        }

        // 通常将 brk 对齐到操作系统的页大小 (Page Size, 一般为 4096)
        let page_size = 4096;
        let p: u64 = max_vaddr; // 引入匿名中间变量 p 表示未对齐的最高地址
        ctx.program_break = (p + page_size - 1) & !(page_size - 1);

        ctx
    }
}
