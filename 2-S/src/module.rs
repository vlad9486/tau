use core::{
    num::NonZero,
    sync::atomic::{AtomicUsize, Ordering},
};

use super::vmem;

enum TableCell<const SIZE: usize> {
    /// The first and only invocation is from supervisor
    Supervisor,
    /// The address space root of the invocation
    Root(vmem::Root),
    /// The stem physical page of the dependency
    Stem(usize),
    /// The index of next free entries in the table
    Index(u16),
    /// No more free entries in the table
    IndexLast,
}

impl<const SIZE: usize> TableCell<SIZE> {
    const fn encode(self) -> usize {
        match self {
            // correspond to root with physical page zero, which is not used
            Self::Supervisor => 9 << 60,
            Self::Root(root) => root.0.get(),
            // last bit of physical address is not used
            Self::Stem(addr) => (1 << 63) + addr,
            Self::Index(idx) => idx as usize,
            Self::IndexLast => SIZE,
        }
    }

    const fn decode(v: usize) -> Self {
        if v == 9 << 60 {
            Self::Supervisor
        } else if v & (1 << 63) != 0 {
            Self::Stem(v ^ (1 << 63))
        } else if v == SIZE {
            Self::IndexLast
        } else if v < SIZE {
            Self::Index(v as u16)
        } else {
            Self::Root(vmem::Root(unsafe { NonZero::new_unchecked(v) }))
        }
    }
}

#[repr(C)]
pub struct Table<const SIZE: usize>([AtomicUsize; SIZE]);

impl<const SIZE: usize> Table<SIZE> {
    fn init(&self) {
        self.head()
            .store(TableCell::<SIZE>::Index(1).encode(), Ordering::Relaxed);
        for i in 1..(SIZE - 1) {
            let cell = TableCell::<SIZE>::Index(i as u16 + 1);
            self.0[i].store(cell.encode(), Ordering::Relaxed);
        }
        // the end of the list
        self.0[SIZE - 1].store(TableCell::<SIZE>::IndexLast.encode(), Ordering::Relaxed);
    }

    fn head(&self) -> &AtomicUsize {
        &self.0[0]
    }

    fn insert(&self, cell: TableCell<SIZE>) -> Option<u16> {
        loop {
            let head_ptr = self.head().load(Ordering::Acquire);
            if head_ptr == TableCell::<SIZE>::IndexLast.encode() {
                // the table is full
                return None;
            }

            let next = self.0.get(head_ptr)?.load(Ordering::Relaxed);
            if self
                .head()
                .compare_exchange(head_ptr, next, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                self.0
                    .get(head_ptr)?
                    .store(cell.encode(), Ordering::Release);
                return Some(head_ptr as u16);
            }
        }
    }

    fn remove(&self, id: u16) -> Option<TableCell<SIZE>> {
        let v = self.0[id as usize].load(Ordering::Relaxed);
        let cell = TableCell::<SIZE>::decode(v);
        if matches!(cell, TableCell::Index(_) | TableCell::IndexLast) {
            return None;
        }
        loop {
            let head_ptr = self.head().load(Ordering::Acquire);
            self.0[id as usize].store(head_ptr, Ordering::Relaxed);
            if self
                .head()
                .compare_exchange(head_ptr, id as usize, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                break Some(cell);
            }
        }
    }

    fn get(&self, id: u16) -> Option<TableCell<SIZE>> {
        let v = self.0.get(id as usize)?.load(Ordering::Relaxed);
        let cell = TableCell::<SIZE>::decode(v);
        if matches!(cell, TableCell::Index(_) | TableCell::IndexLast) {
            return None;
        }
        Some(cell)
    }
}

pub struct Dependency {
    pub stem: usize,
}

pub enum Invocation {
    Supervisor,
    Regular { root: vmem::Root },
}

#[repr(C)]
pub struct ModuleTables {
    invocations: Table<0x800>,
    dependencies: Table<0x800>,
}

impl ModuleTables {
    pub fn init(&self) {
        self.invocations.init();
        self.dependencies.init();
    }

    pub fn insert_dependency(&self, dep: Dependency) -> Option<u16> {
        self.dependencies.insert(TableCell::Stem(dep.stem))
    }

    pub fn remove_dependency(&self, slot: u16) {
        self.dependencies.remove(slot);
    }

    pub fn get_dependency(&self, slot: u16) -> Option<Dependency> {
        let cell = self.dependencies.get(slot)?;
        if let TableCell::Stem(stem) = cell {
            Some(Dependency { stem })
        } else {
            None
        }
    }

    pub fn insert_invocation(&self, inv: Invocation) -> Option<u16> {
        let v = match inv {
            Invocation::Supervisor => TableCell::Supervisor,
            Invocation::Regular { root } => TableCell::Root(root),
        };
        self.invocations.insert(v)
    }

    pub fn remove_invocation(&self, inv: u16) -> Option<Invocation> {
        match self.invocations.get(inv)? {
            TableCell::Root(root) => Some(Invocation::Regular { root }),
            TableCell::Supervisor => Some(Invocation::Supervisor),
            _ => None,
        }
    }
}
