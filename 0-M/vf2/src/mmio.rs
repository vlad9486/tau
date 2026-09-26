pub fn read(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}

pub fn write(address: usize, value: u32) {
    unsafe { (address as *mut u32).write_volatile(value) }
}

pub fn update(address: usize, mask: u32, value: u32) {
    write(address, (read(address) & !mask) | (value & mask));
}
