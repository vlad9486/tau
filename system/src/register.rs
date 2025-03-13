use core::{cell::UnsafeCell, mem::ManuallyDrop, ptr};

#[repr(transparent)]
pub struct Register<R, W>(Inner<R, W>);

#[repr(C)]
union Inner<R, W> {
    read: ManuallyDrop<UnsafeCell<R>>,
    write: ManuallyDrop<UnsafeCell<W>>,
}

unsafe impl<R: Send, W: Send> Send for Inner<R, W> {}
unsafe impl<R: Sync, W: Sync> Sync for Inner<R, W> {}

impl<R, W> Register<R, W> {
    #[inline(always)]
    pub fn read(&self) -> R {
        unsafe { ptr::read_volatile(self.0.read.get()) }
    }

    #[inline(always)]
    pub fn write<Q>(&self, value: Q)
    where
        Q: Into<W>,
    {
        unsafe { ptr::write_volatile(self.0.write.get(), value.into()) }
    }
}
