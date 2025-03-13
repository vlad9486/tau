use core::{ffi, fmt};

use super::register::Register;

#[repr(C, align(0x1000))]
pub struct Uart<const UART_STATUS: bool, Word> {
    transmit_holding: Register<Word, Word>,
    interrupt_enable: Register<Word, Word>,
    interrupt_status_fifo_control: Register<Word, Word>,
    line_control: Register<Word, Word>,
    modem_control: Register<Word, Word>,
    line_status: Register<Word, ffi::c_void>,
    modem_status: Register<Word, Word>,
    scratchpad: Register<Word, Word>,
    _hole: [Word; 23],
    uart_status: Register<Word, ffi::c_void>,
}

impl<const UART_STATUS: bool, Word> Uart<UART_STATUS, Word>
where
    Word: From<u8> + Into<u32>,
{
    const UART0_CLOCK_FREQ: u32 = 24_000_000;

    #[inline(always)]
    pub fn init(&self, baud_rate: u32) -> &Self {
        self.set_line_control(0b10000011);
        {
            let divisor = Self::UART0_CLOCK_FREQ / (16 * baud_rate);
            self.transmit_holding.write((divisor & 0xff) as u8);
            self.interrupt_enable.write(((divisor >> 8) & 0xff) as u8);
        }
        self.interrupt_status_fifo_control.write(0b00000110);
        self.interrupt_status_fifo_control.write(0b00000001);

        self.set_line_control(0b00000011);
        self.interrupt_enable.write(0b00000001);
        self
    }

    #[inline(always)]
    fn set_line_control(&self, v: u8) {
        if UART_STATUS {
            while self.uart_status.read().into() & 0b00000001 != 0 {}
        }
        self.line_control.write(v);
    }

    #[inline(always)]
    pub fn interrupt_status(&self) -> u8 {
        self.interrupt_status_fifo_control.read().into() as _
    }

    #[inline(always)]
    pub fn modem_status(&self) -> u8 {
        self.modem_status.read().into() as _
    }

    #[inline(always)]
    pub fn line_status(&self) -> u8 {
        self.line_status.read().into() as _
    }
}

pub trait UartIo {
    fn rx(&self) -> Option<u8>;

    fn tx(&self, b: u8);
}

impl<const UART_STATUS: bool, Word> UartIo for Uart<UART_STATUS, Word>
where
    Word: From<u8> + Into<u32>,
{
    #[inline(always)]
    fn rx(&self) -> Option<u8> {
        if self.line_status.read().into() & 0b00000001 != 0 {
            Some(self.transmit_holding.read().into() as _)
        } else {
            None
        }
    }

    #[inline(always)]
    fn tx(&self, b: u8) {
        while self.line_status.read().into() & 0b00100000 == 0 {}
        self.transmit_holding.write(b);
    }
}

pub struct UartPrinter<'a, T>(pub &'a T)
where
    T: UartIo + ?Sized;

impl<'a, T> fmt::Write for UartPrinter<'a, T>
where
    T: UartIo + ?Sized,
{
    #[inline(always)]
    fn write_char(&mut self, c: char) -> fmt::Result {
        self.0.tx(c as u8);
        Ok(())
    }

    #[inline(always)]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.as_bytes().iter().copied() {
            self.0.tx(b);
        }
        Ok(())
    }
}
