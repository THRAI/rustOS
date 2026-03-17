use crate::mm::vm::VmPage;

pub fn activate_current(_root_pa: usize, _asid: u16) {}

pub fn deactivate_current() {}

pub fn flush_addr_asid(_vaddr: usize, _asid: usize) {}

pub fn create_arch_root_mappings() -> (&'static mut VmPage, &'static mut VmPage) {
    panic!("la64 paging is not implemented")
}
