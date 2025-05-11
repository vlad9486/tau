use core::{
    alloc::{AllocError, Allocator, GlobalAlloc, Layout},
    num::NonZeroUsize,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use super::ubi::Ubi;

#[global_allocator]
static ALLOCATOR: Bump = Bump::new();

// TODO: proper allocator
struct Bump {
    base: AtomicUsize,
}

impl Bump {
    const fn new() -> Self {
        Bump {
            base: AtomicUsize::new(0x0100_0000),
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
}

unsafe impl Allocator for Area {
    fn allocate(&self, layout: Layout) -> Result<ptr::NonNull<[u8]>, AllocError> {
        if layout.size() > self.len {
            return Err(AllocError);
        }
        let start = ALLOCATOR.alloc_unmapped(layout);
        Ubi::map(
            NonZeroUsize::new(self.base),
            start,
            layout.size().div_ceil(0x1000),
        )
        .map_err(|_| AllocError)?;
        let ptr = ptr::NonNull::new(start as _).ok_or(AllocError)?;
        Ok(ptr::NonNull::slice_from_raw_parts(ptr, layout.size()))
    }

    unsafe fn deallocate(&self, ptr: ptr::NonNull<u8>, layout: Layout) {
        let _ = (ptr, layout);
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
        let _ = (ptr, layout);
    }
}
