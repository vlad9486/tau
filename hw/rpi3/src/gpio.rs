use tau_cpu::{RegisterSetField, RegisterMemoryWrite, MemoryMapped};

// Descriptions taken from
// https://github.com/raspberrypi/documentation/files/1888662/BCM2837-ARM-Peripherals.-.Revised.-.V2-1.pdf

/// GPIO Function Select 1
#[allow(non_camel_case_types)]
#[derive(Default, RegisterMemoryWrite, RegisterSetField)]
#[field_enum(FSEL14 = 12..15)]
#[field_enum(FSEL15 = 15..18)]
#[repr(transparent)]
pub struct GPFSEL1(u32);

/// Pin 15
#[allow(non_camel_case_types)]
pub enum FSEL15 {
    Input = 0b000,
    Output = 0b001,
    RXD1 = 0b010, // Mini UART - Alternate function 5
}

/// Pin 14
#[allow(non_camel_case_types)]
pub enum FSEL14 {
    Input = 0b000,
    Output = 0b001,
    TXD1 = 0b010, // Mini UART - Alternate function 5
}

/// GPIO Pull-up/down Clock Register 0
#[allow(non_camel_case_types)]
#[derive(Default, RegisterMemoryWrite, RegisterSetField)]
#[field_enum(PUDCLK14 = 14..15)]
#[field_enum(PUDCLK15 = 15..16)]
#[repr(transparent)]
pub struct GPPUDCLK0(u32);

/// Pin 15
#[allow(non_camel_case_types)]
pub enum PUDCLK15 {
    NoEffect = 0,
    AssertClock = 1,
}

/// Pin 14
#[allow(non_camel_case_types)]
pub enum PUDCLK14 {
    NoEffect = 0,
    AssertClock = 1,
}

#[allow(non_snake_case)]
#[repr(C)]
pub struct Gpio {
    __reserved_0: u32,                          // 0x00
    pub GPFSEL1: MemoryMapped<(), GPFSEL1>,     // 0x04
    __reserved_1: [u32; (0x94 - 0x08) / 4],     // 0x08
    pub GPPUD: MemoryMapped<(), u32>,           // 0x94
    pub GPPUDCLK0: MemoryMapped<(), GPPUDCLK0>, // 0x98
}
