use super::vmem;

#[repr(C)]
pub struct Thread {
    pub registers: [usize; 64],
    pub sepc: usize,
    pub satp: Option<vmem::Root>,
    pub hart: usize,
}

#[repr(C)]
pub struct Scheduler {
    asid_bitmap: [u32; 2048],
}
