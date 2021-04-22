#![no_std]

#[cfg(feature = "macros")]
pub use cando_macros::*;

pub struct Handle(u32);

pub trait UnixLegacy {
    fn main(cmd: String, env: String) -> i32;
    fn signal(code: i8);
}

pub trait Uart {
    fn write(data: &str) -> Result<(), String>;
}
