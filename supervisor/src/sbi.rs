use core::{fmt, hint};

use super::asm;

#[derive(Clone, Copy)]
pub enum SbiError {
    Unknown,
    Failed,
    NotSupported,
    InvalidParam,
    Denied,
    InvalidAddress,
    AlreadyAvailable,
    AlreadyStarted,
    AlreadyStopped,
}

impl SbiError {
    #[inline(always)]
    fn from_code((err, value): (isize, usize)) -> Result<usize, Self> {
        match err {
            0 => Ok(value),
            -1 => Err(Self::Failed),
            -2 => Err(Self::NotSupported),
            -3 => Err(Self::InvalidParam),
            -4 => Err(Self::Denied),
            -5 => Err(Self::InvalidAddress),
            -6 => Err(Self::AlreadyAvailable),
            -7 => Err(Self::AlreadyStarted),
            -8 => Err(Self::AlreadyStopped),
            _ => Err(Self::Unknown),
        }
    }
}

#[inline(always)]
pub fn print_byte(byte: u8) -> Result<usize, SbiError> {
    SbiError::from_code(unsafe { asm::sbi(u32::from_be_bytes(*b"DBCN"), 2, [byte as usize]) })
}

#[inline(always)]
pub fn set_timer(time: usize) -> Result<usize, SbiError> {
    SbiError::from_code(unsafe { asm::sbi(u32::from_be_bytes(*b"TIME"), 0, [time]) })
}

#[inline(always)]
pub fn hart_start(hart_id: usize, start_addr: usize, opaque: usize) -> Result<usize, SbiError> {
    SbiError::from_code(unsafe {
        asm::sbi(
            u32::from_be_bytes(*b"\0HSM"),
            0,
            [hart_id, start_addr, opaque],
        )
    })
}

#[inline(always)]
pub fn hart_stop() -> Result<usize, SbiError> {
    SbiError::from_code(unsafe { asm::sbi(u32::from_be_bytes(*b"\0HSM"), 1, []) })
}

#[inline(always)]
pub fn system_reset() -> ! {
    unsafe {
        asm::sbi(u32::from_be_bytes(*b"SRST"), 0, [0, 0]);
        hint::unreachable_unchecked()
    };
}

pub struct Printer;

impl Printer {
    pub fn ch(self, it: impl IntoIterator<Item = u8>) -> Self {
        for byte in it {
            print_byte(byte).unwrap_or_default();
        }
        self
    }
}

pub fn to_hex(value: usize) -> impl Iterator<Item = u8> {
    (0..16)
        .map(move |i| ((value >> (60 - 4 * i)) & 0xf) as u8)
        .map(|x| if x > 9 { b'a' - 10 + x } else { b'0' + x })
}

pub struct Console;

impl fmt::Write for Console {
    #[inline(always)]
    fn write_char(&mut self, c: char) -> fmt::Result {
        print_byte(c as u8).map_err(|_| fmt::Error).map(drop)
    }

    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            print_byte(b).map_err(|_| fmt::Error)?;
        }

        Ok(())
    }
}
