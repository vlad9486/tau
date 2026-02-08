use core::{
    alloc::{GlobalAlloc, Layout},
    mem,
    num::NonZeroUsize,
    ptr, slice,
    sync::atomic::{AtomicUsize, Ordering},
};

use super::ubi::Ubi;

// STATUS: starts at 0x0100_0000 = 16 MiB
#[global_allocator]
static ALLOCATOR: Bump = Bump::new(0x0100_0000);

// TODO: need proper allocator
struct Bump {
    base: AtomicUsize,
}

impl Bump {
    const fn new(offset: usize) -> Self {
        Bump {
            base: AtomicUsize::new(offset),
        }
    }

    fn alloc_unmapped(&self, layout: Layout) -> usize {
        self.base
            .fetch_add(layout.size().div_ceil(0x1000) * 0x1000, Ordering::SeqCst)
    }
}

pub struct Area {
    base: usize,
    len: usize,
}

impl Area {
    pub const fn new(base: usize, len: usize) -> Self {
        let len = len.div_ceil(0x1000) << 12;
        Area { base, len }
    }

    fn alloc(&self, layout: Layout) -> usize {
        let start = ALLOCATOR.alloc_unmapped(layout);
        Ubi::map(
            NonZeroUsize::new(self.base),
            start,
            layout.size().div_ceil(0x1000),
        )
        .expect("failed to map page");
        start
    }

    pub fn sl<T>(self) -> &'static [T] {
        let addr = self
            .alloc(unsafe { Layout::from_size_align_unchecked(self.len, mem::align_of::<T>()) });
        unsafe {
            slice::from_raw_parts(
                ptr::with_exposed_provenance(addr),
                self.len / mem::size_of::<T>(),
            )
        }
    }

    pub fn r<T>(self) -> &'static T {
        let addr = self.alloc(Layout::new::<T>());
        if mem::size_of::<T>() <= self.len {
            unsafe { &*ptr::with_exposed_provenance(addr) }
        } else {
            panic!()
        }
    }
}

unsafe impl GlobalAlloc for Bump {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let start = self.alloc_unmapped(layout);
        match Ubi::map(None, start, layout.size().div_ceil(0x1000)) {
            Ok(()) => start as _,
            Err(_) => ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Ubi::unmap(ptr.addr(), layout.size().div_ceil(0x1000)).unwrap_or_default();
    }
}
