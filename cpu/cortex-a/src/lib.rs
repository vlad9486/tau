#![no_std]
#![feature(asm)]

pub mod asm;

#[cfg(target_arch = "aarch64")]
pub mod regs;
