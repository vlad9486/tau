use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
};

use super::vmem;

pub struct Invocation {
    pub satp: vmem::Root,
    pub sepc: usize,
    pub sp: usize,
}

#[repr(C)]
struct InvocationEntry {
    a: AtomicUsize,
    b: AtomicUsize,
    c: AtomicUsize,
}

#[repr(C)]
pub struct InvocationsTable([InvocationEntry; INVOCATIONS]);

const INVOCATIONS: usize = 0x400;

impl InvocationsTable {
    pub fn init(&self) {
        for i in 1..(INVOCATIONS - 1) {
            self.0[i].b.store(i + 1, Ordering::Relaxed);
        }
        self.0[INVOCATIONS - 1]
            .b
            .store(usize::MAX, Ordering::Relaxed);
        self.head().store(1, Ordering::Relaxed);
    }

    fn head(&self) -> &AtomicUsize {
        &self.0[0].b
    }

    pub fn insert(&self, invocation: Invocation) -> Option<u16> {
        loop {
            let head_ptr = self.head().load(Ordering::Acquire);
            if head_ptr == usize::MAX {
                return None;
            }

            let next = self.0.get(head_ptr)?.b.load(Ordering::Relaxed);
            if self
                .head()
                .compare_exchange(head_ptr, next, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                let this = self.0.get(head_ptr)?;
                let Invocation { satp, sepc, sp } = invocation;
                this.a.store(satp.0.get(), Ordering::Release);
                this.b.store(sepc, Ordering::Relaxed);
                this.c.store(sp, Ordering::Relaxed);
                return Some(head_ptr as u16);
            }
        }
    }

    pub fn remove(&self, id: u16) {
        loop {
            let head_ptr = self.head().load(Ordering::Acquire);
            self.0[id as usize].b.store(head_ptr, Ordering::Relaxed);
            self.0[id as usize].a.store(0, Ordering::Release); // Mark as free
            if self
                .head()
                .compare_exchange(head_ptr, id as usize, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
    }

    pub fn get(&self, id: u16) -> Option<Invocation> {
        let InvocationEntry { a, b, c } = self.0.get(id as usize)?;
        let satp = NonZeroUsize::new(a.load(Ordering::Relaxed)).map(vmem::Root)?;
        let sepc = b.load(Ordering::Relaxed);
        let sp = c.load(Ordering::Relaxed);
        Some(Invocation { satp, sepc, sp })
    }
}

pub struct Dependency {
    pub stem: usize,
    pub sepc: usize,
}

#[repr(C)]
struct DependencyEntry {
    stem: AtomicUsize,
    sepc: AtomicUsize,
}

#[repr(C)]
pub struct DependenciesTable([DependencyEntry; DEPENDENCIES]);

const DEPENDENCIES: usize = 0x200;

impl DependenciesTable {
    pub fn put(&self, slot: u16, dependency: Dependency) {
        if let Some(item) = self.0.get(slot as usize) {
            item.stem.store(dependency.stem, Ordering::Relaxed);
            item.sepc.store(dependency.sepc, Ordering::Relaxed);
        }
    }

    pub fn get(&self, slot: u16) -> Option<Dependency> {
        let item = self.0.get(slot as usize)?;
        let stem = item.stem.load(Ordering::Relaxed);
        let sepc = item.sepc.load(Ordering::Relaxed);
        Some(Dependency { stem, sepc })
    }
}

#[repr(C)]
pub struct ModuleTables {
    pub invocations: InvocationsTable,
    pub dependencies: DependenciesTable,
}
