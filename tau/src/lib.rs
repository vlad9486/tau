#![no_std]
#![cfg_attr(
    feature = "nightly",
    feature(custom_test_frameworks),
    test_runner(tester::test_runner)
)]

pub mod tester;

pub mod asm;

mod common;
pub use self::common::*;

pub mod loader;

mod ubi;
pub use self::ubi::{Ubi, FreeError, AllocError};

mod dtb;
pub use self::dtb::{Dtb, DtbProps, DtbHeaderError};

mod heap;
pub use self::heap::Area;

pub fn to_size<T>(v: T) -> usize
where
    T: Into<u32>,
{
    #[cfg(not(any(target_pointer_width = "32", target_pointer_width = "64")))]
    compile_error!("Tau requires 32- or 64-bit targets");

    unsafe { usize::try_from(v.into()).unwrap_unchecked() }
}
