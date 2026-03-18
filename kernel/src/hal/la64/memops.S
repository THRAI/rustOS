# memops.S -- la64 memset/memcpy/memmove placeholders
#
# Early la64 bring-up currently relies on compiler-builtins / Rust lowering for
# memory intrinsics. This file intentionally exists so `main.rs` can dispatch
# `global_asm!` by architecture without reusing the rv64 path.
