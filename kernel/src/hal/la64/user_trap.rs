use alloc::sync::Arc;

use crate::proc::Task;

pub fn trap_return(_task: &Arc<Task>) {
    panic!("la64 trap_return is not implemented");
}
