#![no_std]

use core::{cell::UnsafeCell, ptr, mem::ManuallyDrop};

pub trait RegisterRead {
    fn get_value() -> Self;
}

pub trait RegisterWrite {
    fn set_value(self);
}

pub trait RegisterGetField<F> {
    fn get_field(&self) -> F;
}

pub trait RegisterSetField<F> {
    fn set_field(self, field: F) -> Self;
}

#[repr(transparent)]
pub struct MemoryMapped<R, W>(UnsafeCell<Register<R, W>>);

#[repr(C)]
union Register<R, W> {
    read: ManuallyDrop<R>,
    write: ManuallyDrop<W>,
}

pub trait RegisterMemoryRead
where
    Self: Sized,
{
    fn read_value<W>(memory_mapped: &MemoryMapped<Self, W>) -> Self {
        unsafe {
            let value = ptr::read_volatile(memory_mapped.0.get());
            ManuallyDrop::into_inner(value.read)
        }
    }
}

pub trait RegisterMemoryWrite
where
    Self: Sized,
{
    fn write_value<R>(self, memory_mapped: &MemoryMapped<R, Self>) {
        let value = Register {
            write: ManuallyDrop::new(self),
        };
        unsafe { ptr::write_volatile(memory_mapped.0.get(), value) }
    }
}

impl RegisterMemoryRead for u32 {}

impl RegisterMemoryWrite for u32 {}

impl RegisterMemoryRead for u64 {}

impl RegisterMemoryWrite for u64 {}
