use core::fmt;
use tau_cpu::{
    MemoryMapped, RegisterMemoryRead, RegisterGetField, RegisterMemoryWrite, RegisterSetField,
    cortex_a::asm,
};

use super::gpio::{Gpio, GPFSEL1, FSEL15, FSEL14, GPPUDCLK0, PUDCLK15, PUDCLK14};

/// Auxiliary enables
#[allow(non_camel_case_types)]
#[derive(Default, RegisterMemoryWrite, RegisterSetField)]
#[field_bool(MINI_UART_ENABLE = 0)]
#[repr(transparent)]
struct AUX_ENABLES(u32);

#[allow(non_camel_case_types)]
struct MINI_UART_ENABLE(pub bool);

/// Mini Uart Interrupt Identify
#[allow(non_camel_case_types)]
#[derive(Default, RegisterMemoryWrite, RegisterSetField)]
#[field_enum(FIFO_CLEAR = 1..3)]
#[repr(transparent)]
struct AUX_MU_IIR(u32);

/// Writing with bit 1 set will clear the receive FIFO
/// Writing with bit 2 set will clear the transmit FIFO
#[allow(non_camel_case_types, dead_code)]
enum FIFO_CLEAR {
    Rx = 0b01,
    Tx = 0b10,
    All = 0b11,
}

/// Mini Uart Line Control
#[allow(non_camel_case_types)]
#[derive(Default, RegisterMemoryWrite, RegisterSetField)]
#[field_enum(DATA_SIZE = 0..2)]
#[repr(transparent)]
struct AUX_MU_LCR(u32);

/// Mode the UART works in
#[allow(non_camel_case_types, dead_code)]
enum DATA_SIZE {
    SevenBit = 0b00,
    EightBit = 0b11,
}

/// Mini Uart Line Status
#[allow(non_camel_case_types)]
#[derive(RegisterMemoryRead, RegisterGetField)]
#[field_bool(DATA_READY = 0)]
#[field_bool(TX_EMPTY = 5)]
#[repr(transparent)]
struct AUX_MU_LSR(u32);

/// This bit is set if the transmit FIFO can accept at least
/// one byte.
#[allow(non_camel_case_types)]
struct TX_EMPTY(pub bool);

/// This bit is set if the receive FIFO holds at least 1
/// symbol.
#[allow(non_camel_case_types)]
struct DATA_READY(pub bool);

/// Mini Uart Extra Control
#[allow(non_camel_case_types)]
#[derive(Default, RegisterMemoryWrite, RegisterSetField)]
#[field_bool(RX_EN = 0)]
#[field_bool(TX_EN = 1)]
#[repr(transparent)]
struct AUX_MU_CNTL(u32);

/// If this bit is set the mini UART transmitter is enabled.
/// If this bit is clear the mini UART transmitter is disabled.
#[allow(non_camel_case_types)]
struct TX_EN(pub bool);

/// If this bit is set the mini UART receiver is enabled.
/// If this bit is clear the mini UART receiver is disabled.
#[allow(non_camel_case_types)]
struct RX_EN(pub bool);

/// Mini Uart Baud rate
#[allow(non_camel_case_types)]
#[derive(Default, RegisterMemoryWrite, RegisterSetField)]
#[field_int(RATE = 0..16)]
#[repr(transparent)]
struct AUX_MU_BAUD(u32);

/// Mini UART baud rate counter
#[allow(non_camel_case_types)]
struct RATE(pub u16);

#[repr(C)]
pub struct MiniUart {
    __reserved_0: u32,                              // 0x00
    enable: MemoryMapped<AUX_ENABLES, AUX_ENABLES>, // 0x04
    __reserved_1: [u32; 14],                        // 0x08
    mu_io: MemoryMapped<u32, u32>,                  // 0x40 - Mini Uart I/O Data
    mu_ier: MemoryMapped<(), u32>,                  // 0x44 - Mini Uart Interrupt Enable
    mu_iir: MemoryMapped<(), AUX_MU_IIR>,           // 0x48
    mu_lcr: MemoryMapped<(), AUX_MU_LCR>,           // 0x4C
    mu_mcr: MemoryMapped<(), u32>,                  // 0x50
    mu_lsr: MemoryMapped<AUX_MU_LSR, ()>,           // 0x54
    __reserved_2: [u32; 2],                         // 0x58
    mu_cntl: MemoryMapped<(), AUX_MU_CNTL>,         // 0x60
    __reserved_3: u32,                              // 0x64
    mu_baud: MemoryMapped<(), AUX_MU_BAUD>,         // 0x68
}

impl MiniUart {
    ///Set baud rate and characteristics (115200 8N1) and map to GPIO
    pub fn init(&mut self, gpio: &mut Gpio) {
        // initialize UART
        AUX_ENABLES::default()
            .set_field(MINI_UART_ENABLE(true))
            .write_value(&self.enable);
        0u32.write_value(&self.mu_ier);
        AUX_MU_CNTL::default().write_value(&self.mu_cntl);
        AUX_MU_LCR::default()
            .set_field(DATA_SIZE::EightBit)
            .write_value(&self.mu_lcr);
        0u32.write_value(&self.mu_mcr);
        0u32.write_value(&self.mu_ier);
        AUX_MU_IIR::default()
            .set_field(FIFO_CLEAR::All)
            .write_value(&self.mu_iir);
        AUX_MU_BAUD::default()
            .set_field(RATE(270))
            .write_value(&self.mu_baud);

        // map UART1 to GPIO pins
        GPFSEL1::default()
            .set_field(FSEL15::RXD1)
            .set_field(FSEL14::TXD1)
            .write_value(&gpio.GPFSEL1);
        0u32.write_value(&gpio.GPPUD);
        for _ in 0..150 {
            asm::nop();
        }

        GPPUDCLK0::default()
            .set_field(PUDCLK15::AssertClock)
            .set_field(PUDCLK14::AssertClock)
            .write_value(&gpio.GPPUDCLK0);
        for _ in 0..150 {
            asm::nop();
        }
        GPPUDCLK0::default().write_value(&gpio.GPPUDCLK0);

        AUX_MU_CNTL::default()
            .set_field(RX_EN(true))
            .set_field(TX_EN(true))
            .write_value(&self.mu_cntl);
    }

    /// Send a character
    pub fn send(&mut self, c: char) {
        // wait until we can send
        while let TX_EMPTY(false) = <_>::read_value(&self.mu_lsr).get_field() {
            asm::nop();
        }

        // write the character to the buffer
        (c as u32).write_value(&self.mu_io);
    }

    /// Receive a character
    pub fn receive(&self) -> char {
        // wait until something is in the buffer
        while let DATA_READY(false) = <_>::read_value(&self.mu_lsr).get_field() {
            asm::nop();
        }

        u32::read_value(&self.mu_io) as u8 as _
    }
}

impl fmt::Write for MiniUart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        s.chars().for_each(|c| self.send(c));
        Ok(())
    }
}
