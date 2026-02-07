use core::{mem::MaybeUninit, num::NonZeroUsize, ptr};

use super::{llfree, asm, cpu};

pub const SV39: usize = 8;

pub const fn flags_new(&[r, w, x, u, g]: &[u8; 5]) -> usize {
    let mut f = 0b1100_0001;
    if r != b'-' {
        f |= 1 << 1
    };
    if w != b'-' {
        f |= 1 << 2
    };
    if x != b'-' {
        f |= 1 << 3
    };
    if u != b'-' {
        f |= 1 << 4
    };
    if g != b'-' {
        f |= 1 << 5
    };
    f
}

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Root(pub NonZeroUsize);

impl Root {
    #[inline]
    pub fn sv39(ptr: usize, asid: u16) -> Self {
        let v = (SV39 << 60) | ((asid as usize) << 44) | (ptr >> 12);
        // Safety: `v` is never zero by construction
        Self(unsafe { NonZeroUsize::new_unchecked(v) })
    }

    #[inline]
    pub fn asid(self) -> u16 {
        ((self.0.get() >> 44) & 0xffff) as u16
    }

    #[inline]
    pub fn page(self) -> usize {
        (self.0.get() << 20) >> 8
    }
}

#[derive(Default, Clone, Copy)]
pub struct Mapping {
    pub addr_virtual: usize,
    pub virtual_pages: usize,
    pub addr_physical: Option<NonZeroUsize>,
    pub physical_pages: usize,
    pub flags: [u8; 5],
}

#[repr(C)]
struct Table([Option<NonZeroUsize>; 0o1000]);

impl Table {
    #[inline]
    fn temp_map(&mut self, pos: usize, write: bool, addr_physical: usize, asid: u16) {
        let mut flags = flags_new(b"r----");
        if write {
            flags |= 1 << 2;
        }
        let v = Some(unsafe { NonZeroUsize::new_unchecked(((addr_physical >> 12) << 10) | flags) });
        let ptr = self.0.get_mut(pos & 0o777).expect("cannot fail");
        if v != *ptr {
            unsafe { ptr::from_mut(ptr).write_volatile(v) };
            let addr = ptr::from_ref(self).addr() + pos * 0x1000;
            asm::sfence_vma(Some(addr), Some(asid));
        }
    }
}

#[repr(C)]
pub struct Window {
    table: Table,
    root: MaybeUninit<[Option<NonZeroUsize>; 0o1000]>,
    stem: MaybeUninit<[Option<NonZeroUsize>; 0o1000]>,
    leafs: [MaybeUninit<[Option<NonZeroUsize>; 0o1000]>; 0o675],
}

const _: () = assert!(
    size_of::<Window>() <= 1 << 21,
    "size of window must not exceed 2 MiB"
);

impl Window {
    pub fn current_root() -> Root {
        let satp = cpu::csrr!("satp");
        Root(unsafe { NonZeroUsize::new_unchecked(satp) })
    }

    pub fn init(&mut self, root: Root) {
        let asid = root.asid();

        const POS: usize = memoffset::offset_of!(Window, root) >> 12;
        self.table.temp_map(POS, true, root.page(), asid);
    }

    // pub fn create<F>(&mut self, current_asid: u16, root: Root, hart_id: usize, mut get_table: F)
    // where
    //     F: FnMut() -> usize,
    // {
    //     let addr_virtual = config::window_addr(hart_id);

    //     self.table
    //         .temp_map_root_next(true, root.get_page(), current_asid);

    //     let root_table = unsafe { self.root_next.assume_init_mut() };
    //     let ri = (addr_virtual >> 30) & 0o777;

    //     let stem_table = if let Some(v) = root_table[ri] {
    //         self.table
    //             .temp_map_stem_next(true, (v.get() >> 10) << 12, current_asid);
    //         unsafe { self.stem_next.assume_init_mut() }
    //     } else {
    //         let addr_physical = get_table();
    //         root_table[ri] =
    //             Some(unsafe { NonZeroUsize::new_unchecked(addr_physical >> 2 | flags_empty()) });
    //         self.table
    //             .temp_map_stem_next(true, addr_physical, current_asid);
    //         self.stem_next.write([None; 512])
    //     };
    //     let si = (addr_virtual >> 21) & 0o777;

    //     let (leaf_table, addr_physical) = if let Some(v) = stem_table[si] {
    //         let addr_physical = (v.get() >> 10) << 12;
    //         self.table
    //             .temp_map_leaf(0, true, addr_physical, current_asid);
    //         (unsafe { self.leafs[0].assume_init_mut() }, addr_physical)
    //     } else {
    //         let addr_physical = get_table();
    //         stem_table[si] =
    //             Some(unsafe { NonZeroUsize::new_unchecked(addr_physical >> 2 | flags_empty()) });
    //         self.table
    //             .temp_map_leaf(0, true, addr_physical, current_asid);
    //         (self.leafs[0].write([None; 512]), addr_physical)
    //     };

    //     leaf_table[0] =
    //         Some(unsafe { NonZeroUsize::new_unchecked(addr_physical >> 2 | flags_new(b"rw--g")) });

    //     leaf_table[1] = Some(unsafe {
    //         NonZeroUsize::new_unchecked(root.get_page() >> 2 | flags_new(b"rw--g"))
    //     });

    //     asm::sfence_vma(
    //         Some(((&mut self.table) as *mut WindowTable).addr()),
    //         Some(current_asid),
    //     );
    // }

    pub fn create(
        &mut self,
        asid: u16,
        new_asid: u16,
        page: impl Fn() -> Result<usize, llfree::Error>,
    ) -> Result<Root, llfree::Error> {
        let addr_virtual = ptr::from_ref(self).addr();

        let addr_physical = page()?;
        let root = Root::sv39(addr_physical, new_asid);
        let pos = memoffset::offset_of!(Window, root) >> 12;
        self.table.temp_map(pos, true, addr_physical, asid);

        let root_table = unsafe { self.root.assume_init_mut() };
        let ri = (addr_virtual >> 30) & 0o777;

        let addr_physical = page()?;
        root_table[ri] = Some(unsafe { NonZeroUsize::new_unchecked((addr_physical >> 2) | 1) });
        let pos = memoffset::offset_of!(Window, stem) >> 12;
        self.table.temp_map(pos, true, addr_physical, asid);

        let stem_table = self.stem.write([None; 512]);
        let si = (addr_virtual >> 21) & 0o777;

        // TODO: create new address space
        let _ = (stem_table, si);

        Ok(root)
    }

    // TODO: deallocate steam and leaf table if needed
    pub fn unmap(
        &mut self,
        current_asid: u16,
        addr_virtual: usize,
        free: impl Fn(usize) -> Result<(), llfree::Error>,
    ) -> Result<(), llfree::Error> {
        // warning, assuming the root is initialized
        let table = unsafe { self.root.assume_init_ref() };
        let ri = (addr_virtual >> 30) & 0o777;

        let Some(v) = table[ri] else {
            return Ok(());
        };
        let pos = memoffset::offset_of!(Window, stem) >> 12;
        self.table
            .temp_map(pos, false, (v.get() >> 10) << 12, current_asid);

        let table = unsafe { self.root.assume_init_ref() };
        let si = (addr_virtual >> 21) & 0o777;

        let Some(v) = table[si] else {
            return Ok(());
        };
        let pos = memoffset::offset_of!(Window, leafs) >> 12;
        self.table
            .temp_map(pos, false, (v.get() >> 10) << 12, current_asid);

        let table = unsafe { self.leafs[0].assume_init_mut() };
        let li = (addr_virtual >> 12) & 0o777;

        let Some(v) = table[li].take() else {
            return Ok(());
        };
        free(v.get())
    }

    // Root must be initialized
    pub fn map(
        &mut self,
        asid: u16,
        mapping: Mapping,
        page: impl Fn() -> Result<usize, llfree::Error>,
    ) -> Result<(), llfree::Error> {
        let root_table = unsafe { self.root.assume_init_mut() };
        let ri = (mapping.addr_virtual >> 30) & 0o777;
        let stem_table = if let Some(v) = root_table[ri] {
            let pos = memoffset::offset_of!(Window, stem) >> 12;
            self.table.temp_map(pos, true, (v.get() >> 10) << 12, asid);
            unsafe { self.stem.assume_init_mut() }
        } else {
            let phys = page()?;
            root_table[ri] = Some(unsafe { NonZeroUsize::new_unchecked((phys >> 2) | 1) });
            let pos = memoffset::offset_of!(Window, stem) >> 12;
            self.table.temp_map(pos, true, phys, asid);
            self.stem.write([None; 512])
        };

        let si_start = (mapping.addr_virtual >> 21) & 0o777;
        let si_end = {
            let pn = mapping.addr_virtual >> 12;
            let limit = pn + mapping.virtual_pages + 511;
            let e = (limit >> 9) & 0o777;
            if e == 0 { 0o1000 } else { e }
        };

        let mut bitmap = [0_u32; 16];
        for i in 0..(si_end - si_start) {
            let p = unsafe { stem_table.get_unchecked_mut(si_start + i) };
            let phys = if let Some(p) = p {
                ((*p).get() >> 10) << 12
            } else {
                let k = (i / 32) % 16;
                let j = i % 32;
                bitmap[k] |= 1 << j;
                let phys = page()?;
                *p = Some(unsafe { NonZeroUsize::new_unchecked((phys >> 2) | 1) });
                phys
            };

            let pos = memoffset::offset_of!(Window, leafs) >> 12;
            self.table.temp_map(pos + i, true, phys, asid);
        }

        for i in 0..(si_end - si_start) {
            let k = (i / 32) % 16;
            let j = i % 32;
            if bitmap[k] & (1 << j) != 0 {
                unsafe { self.leafs.get_unchecked_mut(i).write([None; 512]) };
            }
        }

        let limit = (mapping.addr_virtual >> 12) + mapping.virtual_pages;
        let mut p_virtual = mapping.addr_virtual >> 12;
        let mut addr_physical = mapping.addr_physical;
        while p_virtual < limit {
            let si = (p_virtual >> 9) & 0o777;
            let table = unsafe {
                self.leafs
                    .get_unchecked_mut(si - si_start)
                    .assume_init_mut()
            };

            let addr = if let Some(addr) = &mut addr_physical {
                let v = addr.get();
                *addr = addr.saturating_add(0x1000);
                let initial = mapping
                    .addr_physical
                    .map(NonZeroUsize::get)
                    .unwrap_or_default();
                if addr.get() >= initial + (mapping.physical_pages << 12) {
                    addr_physical = None;
                }
                v
            } else {
                page()?
            };

            let li = p_virtual & 0o777;
            table[li] = Some(unsafe {
                NonZeroUsize::new_unchecked((addr >> 2) | flags_new(&mapping.flags))
            });

            p_virtual = p_virtual.wrapping_add(1);
        }

        Ok(())
    }
}
