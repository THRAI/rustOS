#![cfg(all(target_arch = "loongarch64", feature = "la64-bringup"))]

use alloc::{format, string::String, sync::Arc};
use core::{arch::global_asm, slice};

use crate::{
    executor,
    hal_common::{Errno, PhysAddr, VirtAddr, PAGE_SIZE},
    mm::{
        self, pmap_enter, pmap_extract, pmap_extract_with_flags,
        vm::{PageRef, VObjIndex, VmMapping, VmObject},
        PageRole,
    },
    proc::Task,
};

const USER_SMOKE_ENTRY: usize = 0x0000_0000_0001_0000;
const USER_SMOKE_STACK_TOP: usize = 0x0000_003F_FFFF_F000;
const USER_SMOKE_STACK_BOTTOM: usize = USER_SMOKE_STACK_TOP - PAGE_SIZE;

global_asm!(include_str!("hal/la64/bringup_smoke_user.S"));

extern "C" {
    static __la64_user_smoke_start: u8;
    static __la64_user_smoke_end: u8;
}

pub fn spawn_runtime_probe(cpu: usize) {
    executor::spawn_kernel_task(
        async move {
            crate::kprintln!("la64 executor online");
            let mut spin: u64 = 0;
            let mut syscall_marker_reported = false;
            loop {
                executor::yield_now().await;
                spin = spin.wrapping_add(1);
                if spin % 5_000_000 == 0 {
                    let ticks = crate::hal::la64::time::tick_count();
                    let timer_enters = crate::hal::la64::time::timer_handler_enter_count();
                    let (k, u) = crate::hal::la64::trap::trap_counts();
                    let (t, s, e, y, f) = crate::hal::la64::trap::trap_cause_counts();
                    if y > 0 && !syscall_marker_reported {
                        crate::kprintln!("la64 kernel syscall round-trip marker");
                        syscall_marker_reported = true;
                    }
                    crate::kprintln!(
                        "la64 runtime probe: spin={} timer_enters={} ticks={} traps=({},{}) causes(t,s,e,y,f)=({},{},{},{},{})",
                        spin,
                        timer_enters,
                        ticks,
                        k,
                        u,
                        t,
                        s,
                        e,
                        y,
                        f,
                    );
                }
            }
        },
        cpu,
    )
    .detach();
}

pub fn spawn_builtin_user_smoke(cpu: usize) {
    crate::kprintln!("la64 user smoke: creating init task");
    let task = Task::new_init();
    crate::kprintln!("la64 user smoke: init task created pid={}", task.pid);
    let pid = task.pid;
    let smoke_entry = USER_SMOKE_ENTRY + smoke_text_page_offset();
    match prepare_builtin_user_smoke(&task) {
        Ok(()) => {
            {
                let mut tf = task.trap_frame.lock();
                crate::hal::syscall_abi::setup_exec(
                    &mut tf,
                    smoke_entry,
                    USER_SMOKE_STACK_TOP,
                    0,
                    0,
                    0,
                );
                // Keep the built-in smoke path deterministic while LA64 timer
                // interrupt return semantics are still being stabilized.
                let user_status = tf.status() & !(1 << 2);
                tf.set_status(user_status);
            }
            crate::kprintln!(
                "la64 built-in user smoke spawned pid={} entry={:#x} sp={:#x}",
                pid,
                smoke_entry,
                USER_SMOKE_STACK_TOP
            );
            executor::spawn_user_task(task, cpu);
        },
        Err(err) => {
            crate::kprintln!("la64 built-in user smoke setup failed: {:?}", err);
        },
    }
}

fn prepare_builtin_user_smoke(task: &Arc<Task>) -> Result<(), Errno> {
    let image = smoke_image()?;
    crate::kprintln!("la64 user smoke: image size={} bytes", image.len());
    let smoke_text_page = smoke_text_page_pa();
    crate::kprintln!(
        "la64 user smoke: embedded text page at {:#x}",
        smoke_text_page.as_usize()
    );

    let code_page = alloc_filled_page(PageRole::UserExec, |dst| {
        dst[..image.len()].copy_from_slice(image);
    })?;
    crate::kprintln!(
        "la64 user smoke: code page allocated at {:#x}",
        code_page.as_usize()
    );
    let stack_page = alloc_filled_page(PageRole::UserStack, |dst| {
        dst[PAGE_SIZE - 1] = b'!';
    })?;
    crate::kprintln!(
        "la64 user smoke: stack page allocated at {:#x}",
        stack_page.as_usize()
    );

    let code_obj = VmObject::new_anon(PAGE_SIZE);
    code_obj
        .write()
        .insert_page(VObjIndex::new(0), PageRef::new(code_page));

    let stack_obj = VmObject::new_anon(PAGE_SIZE);
    stack_obj
        .write()
        .insert_page(VObjIndex::new(0), PageRef::new(stack_page));

    let mut vm = task.vm_map.write();
    vm.map(
        VirtAddr::new(USER_SMOKE_ENTRY),
        VirtAddr::new(USER_SMOKE_ENTRY + PAGE_SIZE),
        crate::map_perm!(R, X, U),
        VmMapping::AnonPrivate {
            object: Arc::clone(&code_obj),
            offset: 0,
        },
    )
    .map_err(|_| Errno::Einval)?;
    vm.map(
        VirtAddr::new(USER_SMOKE_STACK_BOTTOM),
        VirtAddr::new(USER_SMOKE_STACK_TOP),
        crate::map_perm!(R, W, U),
        VmMapping::AnonPrivate {
            object: Arc::clone(&stack_obj),
            offset: 0,
        },
    )
    .map_err(|_| Errno::Einval)?;
    crate::kprintln!("la64 user smoke: vm mappings installed");

    {
        let mut pmap = vm.pmap_lock();
        crate::kprintln!("la64 user smoke: eager map code start");
        pmap_enter(
            &mut pmap,
            VirtAddr::new(USER_SMOKE_ENTRY),
            smoke_text_page,
            crate::map_perm!(R, X, U),
            false,
        )
        .map_err(|_| Errno::Enomem)?;
        crate::kprintln!("la64 user smoke: eager map code done");
        crate::kprintln!("la64 user smoke: eager map stack start");
        pmap_enter(
            &mut pmap,
            VirtAddr::new(USER_SMOKE_STACK_BOTTOM),
            stack_page,
            crate::map_perm!(R, W, U),
            false,
        )
        .map_err(|_| Errno::Enomem)?;
        crate::kprintln!("la64 user smoke: eager map stack done");
    }
    crate::kprintln!("la64 user smoke: eager pmap install done");
    {
        let pmap = vm.pmap_lock();
        let kernel_pc = crate::rust_main as usize;
        let trap_pc = crate::hal::la64::trap::kernel_trap_handler as usize;
        let kernel_sp = task.kernel_sp - 8;
        let probes = [
            ("rust_main", kernel_pc),
            ("kernel_trap", trap_pc),
            ("kernel_sp", kernel_sp),
            ("user_text", USER_SMOKE_ENTRY),
            ("user_stack", USER_SMOKE_STACK_BOTTOM),
        ];
        for (name, va) in probes {
            let pa = pmap_extract(&pmap, VirtAddr::new(va))
                .map(|p| p.as_usize())
                .unwrap_or(usize::MAX);
            let flags = match pmap_extract_with_flags(&pmap, VirtAddr::new(va)) {
                Some((_, flags)) => format!("{:?}", flags),
                None => String::from("None"),
            };
            crate::kprintln!(
                "la64 user smoke: probe {} va={:#x} -> pa={:#x} flags={}",
                name,
                va,
                pa,
                flags
            );
        }
    }

    Ok(())
}

fn smoke_image() -> Result<&'static [u8], Errno> {
    let start = unsafe { &__la64_user_smoke_start as *const u8 };
    let end = unsafe { &__la64_user_smoke_end as *const u8 };
    let len = unsafe { end.offset_from(start) as usize };
    if len == 0 || len > PAGE_SIZE {
        return Err(Errno::Enoexec);
    }
    Ok(unsafe { slice::from_raw_parts(start, len) })
}

fn smoke_text_page_pa() -> PhysAddr {
    const LA64_PHYS_MASK: usize = 0x0fff_ffff_ffff_ffff;
    let start = unsafe { &__la64_user_smoke_start as *const u8 as usize };
    PhysAddr::new((start & LA64_PHYS_MASK) & !(PAGE_SIZE - 1))
}

fn smoke_text_page_offset() -> usize {
    const LA64_PHYS_MASK: usize = 0x0fff_ffff_ffff_ffff;
    let start = unsafe { &__la64_user_smoke_start as *const u8 as usize };
    (start & LA64_PHYS_MASK) & (PAGE_SIZE - 1)
}

fn alloc_filled_page(role: PageRole, fill: impl FnOnce(&mut [u8])) -> Result<PhysAddr, Errno> {
    let page = mm::alloc_raw_frame_sync(role).ok_or(Errno::Enomem)?;
    unsafe {
        let ptr = page.into_kernel_vaddr().as_mut_ptr();
        let slice = slice::from_raw_parts_mut(ptr, PAGE_SIZE);
        slice.fill(0);
        fill(slice);
        #[cfg(target_arch = "loongarch64")]
        if role == PageRole::UserExec {
            // LA64 bring-up uses freshly populated executable pages. Make the
            // instruction stream visible before first user fetch.
            core::arch::asm!("dbar 0", "ibar 0", options(nostack, preserves_flags));
        }
    }
    Ok(page)
}
