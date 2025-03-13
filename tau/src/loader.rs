//! Loader memory map

pub const LOADER_OFFSET: usize = 0;
pub const LOADER_SIZE: usize = 0x05;
pub const SUPERVISOR_OFFSET: usize = LOADER_OFFSET + LOADER_SIZE;
pub const SUPERVISOR_SIZE: usize = 0x0b;
pub const SYSTEM_OFFSET: usize = SUPERVISOR_OFFSET + SUPERVISOR_SIZE;
pub const SYSTEM_SIZE: usize = 0x30;
pub const HEAP_START: usize = SYSTEM_OFFSET + SYSTEM_SIZE;
pub const HEAP_END: usize = HEAP_START + 0x10;

// must not cross 2MiB boundary
const _: () = assert!(HEAP_END <= 0x200);
