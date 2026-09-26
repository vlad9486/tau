#[repr(C)]
pub struct Thread {
    pub registers: [usize; 64],
    pub sepc: usize,
}

impl Thread {
    // use zero register place to store hart id
    pub fn set_hart_id(&mut self, hart_id: usize) {
        self.registers[0] = hart_id;
    }

    pub fn hart_id(&self) -> usize {
        self.registers[0]
    }
}

#[repr(C)]
pub struct Scheduler {
    asid_bitmap: [u32; 2048],
}
