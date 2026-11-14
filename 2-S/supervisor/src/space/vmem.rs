// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{
    // alloc::{GlobalAlloc, Layout},
    arch,
    cell::UnsafeCell,
    fmt,
    hint,
    mem::{self, MaybeUninit},
    num::NonZeroUsize,
};

use super::{csr, llfree, Module, ModuleDescriptor};

const SV39: usize = 8;
pub const PAGE: usize = 4096;
pub const SCHEDULER: usize = 0xffff_ffc0_0040_0000;
pub const CONTEXT: usize = 0xffff_ffc0_0060_0000;
pub const SUPERVISOR_TEXT: usize = 0xffff_ffc0_0060_1000;
pub const ALLOCATOR: usize = 0xffff_ffc0_006e_0000;
pub const ALLOCATOR_PAGES: usize = 0x900;
pub const DMA_ALLOCATOR: usize = ALLOCATOR + ALLOCATOR_PAGES * PAGE;
pub const DMA_ALLOCATOR_PAGES: usize = 0x20;
pub const ALLOCATOR_END: usize = DMA_ALLOCATOR + DMA_ALLOCATOR_PAGES * PAGE;
pub const MAX_RAM_FRAMES: usize = 1 << 26;
pub const MAX_DMA_FRAMES: usize = 1 << 19;
pub const MODULE_CONTEXT: usize = 0xffff_ffc0_0021_0000;
pub const MODULE: usize = 0xffff_ffc0_0022_0000;
pub const ROOT_INDEX: u16 = 0;
pub const BINDING_INDEX: u16 = 1;
const SUPERVISOR_INDEX: u16 = 256;
// Sv39 RSW: this entry owns an allocator page (data or a child table).
// Borrowed entries stop reclamation, including whole shared branches.
pub const OWNED: usize = 1 << 8;

const _: () = {
    let Some(ram_frames) = NonZeroUsize::new(MAX_RAM_FRAMES) else {
        panic!();
    };
    let Some(ram_meta_bytes) = llfree::Allocator::expected_size(ram_frames) else {
        panic!();
    };
    assert!(ram_meta_bytes == ALLOCATOR_PAGES * PAGE);

    let Some(dma_frames) = NonZeroUsize::new(MAX_DMA_FRAMES) else {
        panic!();
    };
    let Some(dma_meta_bytes) = llfree::Allocator::expected_size(dma_frames) else {
        panic!();
    };
    assert!(dma_meta_bytes.div_ceil(PAGE) <= DMA_ALLOCATOR_PAGES);
    assert!(DMA_ALLOCATOR == 0xffff_ffc0_00fe_0000);
    assert!(ALLOCATOR_END == 0xffff_ffc0_0100_0000);
};

pub const fn flags_new(&[r, w, x, u, g]: &[u8; 5]) -> usize {
    0xc1 | if r != b'-' { 2 } else { 0 }
        | if w != b'-' { 4 } else { 0 }
        | if x != b'-' { 8 } else { 0 }
        | if u != b'-' { 16 } else { 0 }
        | if g != b'-' { 32 } else { 0 }
}

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Root(pub NonZeroUsize);

impl Root {
    pub fn sv39(ptr: usize, asid: u16) -> Self {
        let v = (SV39 << 60) | (usize::from(asid) << 44) | (ptr >> 12);
        // Safety: `v` is never zero by construction
        Self(unsafe { NonZeroUsize::new_unchecked(v) })
    }

    pub fn asid(self) -> u16 {
        unsafe { u16::try_from((self.0.get() >> 44) & 0xffff).unwrap_unchecked() }
    }

    pub fn page(self) -> usize {
        (self.0.get() & ((1 << 44) - 1)) << 12
    }

    /// Two different roots sharing ASID zero always need a fence.
    #[inline(always)]
    pub fn switch_flush(self, destination: Self) -> bool {
        if (self.asid() | destination.asid()) == 0 {
            hint::cold_path();
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Copy)]
pub struct Mapping {
    pub range: MappingRange,
    pub physical_addr: Option<NonZeroUsize>,
    pub flags: [u8; 5],
}

#[derive(Clone, Copy)]
pub struct MappingRange {
    pub addr: usize,
    pub pages: usize,
}

impl MappingRange {
    pub fn pages(&self) -> impl Iterator<Item = usize> {
        (0..self.pages).map(|i| self.addr + i * PAGE)
    }
}

pub fn user_range(address: usize, bytes: usize) -> bool {
    if bytes == 0 {
        return false;
    }

    let Some(last) = address.checked_add(bytes - 1) else {
        return false;
    };
    // Low user half, or high user half above the privileged 1 GiB branch.
    (address < (1 << 38) && last < (1 << 38))
        || (address >= 0xffff_ffc0_4000_0000 && last >= address)
}

pub fn addr_to_index(addr: usize) -> u16 {
    unsafe { u16::try_from((addr >> 30) & 511).unwrap_unchecked() }
}

fn pte_page(pte: usize) -> usize {
    ((pte >> 10) & ((1 << 44) - 1)) << 12
}

fn branch(page: usize) -> usize {
    (page >> 2) | 1
}

#[repr(C, align(4096))]
pub struct Page(pub [u8; PAGE]);

#[repr(C, align(4096))]
pub struct Padded<T> {
    pub inner: T,
}

#[repr(C, align(4096))]
pub struct Hart {
    mask: u16,
    next_asid: u16,
    pub hart_id: usize,
    pub allocator: llfree::Lower<'static>,
    pub base_addr: usize,
    pub dma_allocator: Option<llfree::Lower<'static>>,
    pub dma_base_addr: usize,
}

impl fmt::Display for Hart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Hart { mask, .. } = self;
        writeln!(f, "ASID mask=0x{mask:04x}")
    }
}

/// Linker-provided layout of the 2 MiB hart-local window. The first page maps
/// itself, slot 7 holds MMU state, and slot 511 is the supervisor stack.
/// Reserved bytes may be unmapped or temporarily aliased; never borrow or copy
/// the whole structure. Access only mapped fields or use `WindowAccess`.
#[repr(C)]
#[rustfmt::skip]
pub struct Window {
    table: UnsafeCell<[usize; 512]>,                                // 1 page  (slot 0: recursive L2)
    pte_l0: UnsafeCell<[usize; 512]>,                               // 1 page  (slot 1: L0 root page-table walk)
    pte_l1: UnsafeCell<[usize; 512]>,                               // 1 page  (slot 2: L1 stem page-table walk)
    pte_l2: UnsafeCell<[usize; 512]>,                               // 1 page  (slot 3: L2 leaf page-table walk)
    page: UnsafeCell<MaybeUninit<Page>>,                            // 1 page  (slot 4: zeroing / segment copy)
    descriptor: UnsafeCell<MaybeUninit<Padded<ModuleDescriptor>>>,  // 1 page  (slot 5: foreign module tables page 0 / descriptor)
    wait: MaybeUninit<[tau::WaitSlot; 64]>,                         // 1 page  (slot 6: wait slots)
    hart: UnsafeCell<MaybeUninit<Hart>>,                            // 1 page  (slot 7: permanent per-hart MMU)
    module: UnsafeCell<MaybeUninit<Module>>,                        // 10 pages (slots 8..18: foreign Module)
    _reserved: UnsafeCell<MaybeUninit<[Page; 493]>>,                // unused window slots
    stack: UnsafeCell<MaybeUninit<Page>>,                           // 1 page  (slot 511: supervisor stack)
}

impl Window {
    pub fn current_root() -> Root {
        Root(unsafe { NonZeroUsize::new_unchecked(csr::r!("satp")) })
    }
}

unsafe extern "C" {
    // Per-hart sparse window and stack; pass its pointer without a whole borrow.
    #[allow(improper_ctypes)]
    static __WINDOW: Window;
}

pub const fn access() -> WindowAccess {
    unsafe { WindowAccess::new(&raw const __WINDOW) }
}

const PTE_L0_SLOT: usize = mem::offset_of!(Window, pte_l0) / PAGE;
const PTE_L1_SLOT: usize = mem::offset_of!(Window, pte_l1) / PAGE;
const PTE_L2_SLOT: usize = mem::offset_of!(Window, pte_l2) / PAGE;
const PAGE_SLOT: usize = mem::offset_of!(Window, page) / PAGE;
const DESCRIPTOR_SLOT: usize = mem::offset_of!(Window, descriptor) / PAGE;
const WAIT_SLOT: usize = mem::offset_of!(Window, wait) / PAGE;
pub const HART_SLOT: usize = mem::offset_of!(Window, hart) / PAGE;
const MODULE_SLOT: usize = mem::offset_of!(Window, module) / PAGE;
pub const STACK_SLOT: usize = mem::offset_of!(Window, stack) / PAGE;

const _: () = {
    assert!(PTE_L0_SLOT == 1);
    assert!(PTE_L1_SLOT == 2);
    assert!(PTE_L2_SLOT == 3);
    assert!(PAGE_SLOT == 4);
    assert!(DESCRIPTOR_SLOT == 5);
    assert!(WAIT_SLOT == 6);
    assert!(HART_SLOT == 7);
    assert!(MODULE_SLOT == 8);
    assert!(STACK_SLOT == 511);
    assert!(size_of::<Window>() == 512 * PAGE);
};

/// Exclusive access to a hart's page table, MMU state and temporary aliases.
/// The handle retains the caller's pointer without borrowing the sparse window
/// or its active stack.
pub struct WindowAccess {
    base: *const Window,
}

// How to do it safely? Should I?
// #[global_allocator]
// static GLOBAL: WindowAccess = access();
//
// unsafe impl Send for WindowAccess {}
// unsafe impl Sync for WindowAccess {}
//
// unsafe impl GlobalAlloc for WindowAccess {
//     unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
//         unimplemented!()
//     }
//
//     unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
//         unimplemented!()
//     }
// }

impl Hart {
    pub const fn new(
        hart_id: usize,
        allocator: llfree::Lower<'static>,
        base_addr: usize,
        dma_allocator: Option<llfree::Lower<'static>>,
        dma_base_addr: usize,
    ) -> Self {
        Self {
            mask: 0,
            next_asid: 1,
            hart_id,
            allocator,
            base_addr,
            dma_allocator,
            dma_base_addr,
        }
    }

    pub fn block_zone(&self, order: u32, zone: tau::MemoryZone) -> Result<usize, llfree::Error> {
        let (allocator, base_addr) = match zone {
            tau::MemoryZone::Normal => (&self.allocator, self.base_addr),
            tau::MemoryZone::Dma => (
                self.dma_allocator.as_ref().ok_or(llfree::Error::Memory)?,
                self.dma_base_addr,
            ),
        };
        let frame = allocator.alloc(self.hart_id, order)?.0;
        Ok(base_addr + (frame << 12))
    }

    pub fn block(&self, order: u32) -> Result<usize, llfree::Error> {
        self.block_zone(order, tau::MemoryZone::Normal)
    }

    pub fn page(&self) -> Result<usize, llfree::Error> {
        self.block(0)
    }

    pub fn free_block_zone(
        &self,
        page: usize,
        order: u32,
        zone: tau::MemoryZone,
    ) -> Result<(), llfree::Error> {
        let (allocator, base_addr) = match zone {
            tau::MemoryZone::Normal => (&self.allocator, self.base_addr),
            tau::MemoryZone::Dma => (
                self.dma_allocator.as_ref().ok_or(llfree::Error::Address)?,
                self.dma_base_addr,
            ),
        };
        let offset = page.checked_sub(base_addr).ok_or(llfree::Error::Address)?;
        if order > llfree::MAX_ORDER || offset & ((1usize << (12 + order)) - 1) != 0 {
            return Err(llfree::Error::Address);
        }
        let frame = offset >> 12;
        let count = 1usize << order;
        if frame
            .checked_add(count)
            .is_none_or(|end| end > allocator.frames())
        {
            return Err(llfree::Error::Address);
        }
        allocator.put(self.hart_id, llfree::FrameId(frame), order)
    }

    pub fn free_block(&self, page: usize, order: u32) -> Result<(), llfree::Error> {
        self.free_block_zone(page, order, tau::MemoryZone::Normal)
    }

    pub fn free(&self, page: usize) -> Result<(), llfree::Error> {
        self.free_block(page, 0)
    }
}

impl WindowAccess {
    /// # Safety
    /// `base` must be aligned and have provenance for the full window layout.
    /// Its table and hart fields must be mapped and initialized (zeroed hart
    /// storage is valid). This handle must have exclusive access to those fields
    /// and the temporary aliases. Every root used while the handle is live must
    /// retain the same window backing.
    pub const unsafe fn new(base: *const Window) -> Self {
        WindowAccess { base }
    }

    pub fn init(&mut self) -> Root {
        let original = Window::current_root();
        let probe = Root::sv39(original.page(), u16::MAX);
        unsafe {
            arch::asm!("csrw satp, {0}", in(reg) probe.0.get(), options(nostack));
        }
        let mask = Window::current_root().asid();
        self.hart_mut().mask = mask;
        let asid = self.allocate_tag().unwrap_or(0);
        let root = Root::sv39(original.page(), asid);
        unsafe {
            arch::asm!("csrw satp, {0}", in(reg) root.0.get(), options(nostack));
        }
        sfence_vma(None, None);

        root
    }

    /// Allocate a hart-local ASID tag for a new context root.
    pub fn allocate_tag(&mut self) -> Option<u16> {
        let hart = self.hart_mut();
        if hart.mask == 0 {
            Some(0)
        } else if hart.next_asid != 0 && hart.next_asid <= hart.mask {
            let tag = hart.next_asid;
            hart.next_asid = tag.wrapping_add(1);
            Some(tag)
        } else {
            None
        }
    }

    /// Release the most recently allocated ASID tag after failed root construction.
    pub fn release_tag(&mut self, tag: u16) {
        if tag != 0 {
            let hart = self.hart_mut();
            debug_assert_eq!(hart.next_asid, tag.wrapping_add(1));
            hart.next_asid = tag;
        }
    }

    pub fn hart(&self) -> &Hart {
        unsafe { (&*UnsafeCell::raw_get(&raw const (*self.base).hart)).assume_init_ref() }
    }

    pub fn hart_mut(&mut self) -> &mut Hart {
        unsafe { (&mut *UnsafeCell::raw_get(&raw const (*self.base).hart)).assume_init_mut() }
    }

    fn alias(&mut self, slot: usize, physical: usize, write: bool) {
        debug_assert!(slot > 0 && slot < 511 && slot != HART_SLOT && physical & 0xfff == 0);
        // TODO: proper flags abstraction
        let pte = (physical >> 2) | flags_new(b"r----") | ((1 << 2) * usize::from(write));
        let target = self.base.addr() + slot * PAGE;
        let table = unsafe { UnsafeCell::raw_get(&raw const (*self.base).table) };
        let cell = unsafe { table.cast::<usize>().add(slot) };
        if unsafe { cell.read_volatile() } != pte {
            unsafe { cell.write_volatile(pte) };
            sfence_vma(Some(target), None);
        }
    }

    fn pte_cell(&mut self, slot: usize, table: usize, index: usize) -> *mut usize {
        debug_assert!(matches!(slot, PTE_L0_SLOT | PTE_L1_SLOT | PTE_L2_SLOT) && index < 512);
        self.alias(slot, table, true);
        let ptr = match slot {
            PTE_L0_SLOT => unsafe { UnsafeCell::raw_get(&raw const (*self.base).pte_l0) },
            PTE_L1_SLOT => unsafe { UnsafeCell::raw_get(&raw const (*self.base).pte_l1) },
            _ => unsafe { UnsafeCell::raw_get(&raw const (*self.base).pte_l2) },
        };
        unsafe { ptr.cast::<usize>().add(index) }
    }

    fn read_pte(&mut self, slot: usize, table: usize, index: usize) -> usize {
        unsafe { self.pte_cell(slot, table, index).read_volatile() }
    }

    fn write_pte(&mut self, slot: usize, table: usize, index: usize, pte: usize) {
        unsafe { self.pte_cell(slot, table, index).write_volatile(pte) };
    }

    pub fn new_page(&mut self) -> Result<usize, llfree::Error> {
        let physical = self.hart().page()?;
        self.page_mut(physical).fill(0);
        Ok(physical)
    }

    pub fn create(&mut self, asid: u16) -> Result<Root, llfree::Error> {
        let physical = self.new_page()?;
        Ok(Root::sv39(physical, asid))
    }

    pub fn page_mut(&mut self, physical: usize) -> &mut [u8; PAGE] {
        self.alias(PAGE_SLOT, physical, true);
        let ptr = unsafe { UnsafeCell::raw_get(&raw const (*self.base).page) };
        unsafe { &mut (*ptr).assume_init_mut().0 }
    }

    /// Read the prepared module header from page 0 of foreign module tables.
    pub fn read_module_descriptor(&mut self, physical: usize) -> ModuleDescriptor {
        self.alias(DESCRIPTOR_SLOT, physical, false);
        let ptr = unsafe { UnsafeCell::raw_get(&raw const (*self.base).descriptor) };
        unsafe { (*ptr).assume_init_ref().inner }
    }

    pub fn wait(&mut self, physical: usize) -> &[tau::WaitSlot; 64] {
        self.alias(WAIT_SLOT, physical, false);
        unsafe { (*self.base).wait.assume_init_ref() }
    }

    pub fn map_module(&mut self, physical: usize) -> &mut Module {
        for i in 0..size_of::<Module>().div_ceil(PAGE) {
            self.alias(MODULE_SLOT + i, physical + i * PAGE, true);
        }
        let ptr = unsafe { UnsafeCell::raw_get(&raw const (*self.base).module) };
        unsafe { (&mut *ptr).assume_init_mut() }
    }

    pub fn root_branch(&mut self, root: Root, index: u16) -> Option<usize> {
        let pte = self.read_pte(PTE_L0_SLOT, root.page(), usize::from(index));
        (pte & 1 != 0 && pte & 14 == 0).then(|| pte_page(pte))
    }

    pub fn attach(&mut self, root: Root, index: u16, stem: usize) {
        debug_assert!(self.read_pte(PTE_L0_SLOT, root.page(), usize::from(index)) == 0);
        self.write_pte(PTE_L0_SLOT, root.page(), usize::from(index), branch(stem));
    }

    fn leaf_stem(&mut self, stem: usize, address: usize) -> Result<(usize, usize), llfree::Error> {
        let index = (address >> 21) & 511;
        let pte = self.read_pte(PTE_L1_SLOT, stem, index);
        let table = if pte == 0 {
            let next = self.new_page()?;
            self.write_pte(PTE_L1_SLOT, stem, index, branch(next) | OWNED);
            next
        } else if pte & 1 != 0 && pte & 14 == 0 {
            pte_page(pte)
        } else {
            return Err(llfree::Error::Address);
        };
        Ok((table, (address >> 12) & 511))
    }

    fn leaf(&mut self, root: Root, address: usize) -> Result<(usize, usize), llfree::Error> {
        let ri = (address >> 30) & 511;
        let pte = self.read_pte(PTE_L0_SLOT, root.page(), ri);
        let stem = if pte == 0 {
            let next = self.new_page()?;
            self.write_pte(PTE_L0_SLOT, root.page(), ri, branch(next) | OWNED);
            next
        } else if pte & 1 != 0 && pte & 14 == 0 {
            pte_page(pte)
        } else {
            return Err(llfree::Error::Address);
        };
        self.leaf_stem(stem, address)
    }

    pub fn resolve_stem(&mut self, stem: usize, address: usize) -> Option<(usize, usize)> {
        let mut table = stem;
        for (slot, shift) in [(PTE_L1_SLOT, 21), (PTE_L2_SLOT, 12)] {
            let pte = self.read_pte(slot, table, (address >> shift) & 511);
            if pte & 1 == 0 {
                return None;
            }
            if pte & 14 != 0 {
                let mask = (1 << shift) - 1;
                let physical = pte_page(pte);
                if physical & mask != 0 {
                    return None;
                }
                return Some((physical | (address & mask), pte));
            }
            table = pte_page(pte);
        }
        None
    }

    pub fn resolve(&mut self, root: Root, address: usize) -> Option<(usize, usize)> {
        let mut table = root.page();
        for (slot, shift) in [(PTE_L0_SLOT, 30), (PTE_L1_SLOT, 21), (PTE_L2_SLOT, 12)] {
            let pte = self.read_pte(slot, table, (address >> shift) & 511);
            if pte & 1 == 0 {
                return None;
            }
            if pte & 14 != 0 {
                let mask = (1 << shift) - 1;
                let physical = pte_page(pte);
                if physical & mask != 0 {
                    return None;
                }
                return Some((physical | (address & mask), pte));
            }
            table = pte_page(pte);
        }
        None
    }

    fn check_free_stem(
        &mut self,
        stem: usize,
        mut address: usize,
        mut pages: usize,
    ) -> Result<(), llfree::Error> {
        while pages != 0 {
            let si = (address >> 21) & 511;
            let index = (address >> 12) & 511;
            let chunk = pages.min(512 - index);
            let pte = self.read_pte(PTE_L1_SLOT, stem, si);
            if pte != 0 {
                if pte & 1 == 0 || pte & 14 != 0 {
                    return Err(llfree::Error::Address);
                }
                let leaf = pte_page(pte);
                for offset in 0..chunk {
                    if self.read_pte(PTE_L2_SLOT, leaf, index + offset) != 0 {
                        return Err(llfree::Error::InvalidArgument);
                    }
                }
            }
            address = address.wrapping_add(chunk * PAGE);
            pages -= chunk;
        }
        Ok(())
    }

    fn check_free(
        &mut self,
        root: Root,
        mut address: usize,
        mut pages: usize,
    ) -> Result<(), llfree::Error> {
        while pages != 0 {
            let ri = (address >> 30) & 511;
            let stem_offset = (address >> 12) & ((1 << 18) - 1);
            let chunk = pages.min((1 << 18) - stem_offset);
            let pte = self.read_pte(PTE_L0_SLOT, root.page(), ri);
            if pte != 0 {
                if pte & 1 == 0 || pte & 14 != 0 {
                    return Err(llfree::Error::Address);
                }
                self.check_free_stem(pte_page(pte), address, chunk)?;
            }
            address = address.wrapping_add(chunk * PAGE);
            pages -= chunk;
        }
        Ok(())
    }

    pub fn range_owned(&mut self, root: Root, range: MappingRange) -> bool {
        let MappingRange {
            mut addr,
            mut pages,
        } = range;
        while pages != 0 {
            let ri = addr_to_index(addr);
            let stem_offset = (addr >> 12) & ((1 << 18) - 1);
            let stem_chunk = pages.min((1 << 18) - stem_offset);
            if let Some(stem) = self.root_branch(root, ri) {
                let mut rem = stem_chunk;
                let mut addr = addr;
                while rem != 0 {
                    let si = (addr >> 21) & 511;
                    let index = (addr >> 12) & 511;
                    let chunk = rem.min(512 - index);
                    let pte = self.read_pte(PTE_L1_SLOT, stem, si);
                    if pte & 1 != 0 && pte & 14 == 0 {
                        let leaf = pte_page(pte);
                        for offset in 0..chunk {
                            let entry = self.read_pte(PTE_L2_SLOT, leaf, index + offset);
                            if entry & 1 != 0 && entry & 14 != 0 && entry & OWNED == 0 {
                                return false;
                            }
                        }
                    }
                    addr = addr.wrapping_add(chunk * PAGE);
                    rem -= chunk;
                }
            }
            addr = addr.wrapping_add(stem_chunk * PAGE);
            pages -= stem_chunk;
        }
        true
    }

    pub fn map_stem(&mut self, stem: usize, mapping: Mapping) -> Result<(), llfree::Error> {
        let bytes = mapping
            .range
            .pages
            .checked_mul(PAGE)
            .ok_or(llfree::Error::Address)?;
        if bytes == 0
            || mapping.range.addr & 0xfff != 0
            || mapping.range.addr.checked_add(bytes - 1).is_none()
            || mapping.physical_addr.is_some_and(|p| {
                p.get() & 0xfff != 0
                    || p.get()
                        .checked_add(bytes)
                        .is_none_or(|end| end > (1usize << 56))
            })
        {
            return Err(llfree::Error::Address);
        }
        self.check_free_stem(stem, mapping.range.addr, mapping.range.pages)?;
        let mut table = 0;
        for i in 0..mapping.range.pages {
            let address = mapping.range.addr + i * PAGE;
            let index = (address >> 12) & 511;
            if i == 0 || index == 0 {
                (table, _) = self.leaf_stem(stem, address)?;
            }
            let (physical, owned) = match mapping.physical_addr {
                Some(p) => (p.get() + i * PAGE, 0),
                None => (self.new_page()?, OWNED),
            };
            self.write_pte(
                PTE_L2_SLOT,
                table,
                index,
                (physical >> 2) | flags_new(&mapping.flags) | owned,
            );
        }
        sfence_vma(None, None);
        Ok(())
    }

    pub fn map(&mut self, root: Root, mapping: Mapping) -> Result<(), llfree::Error> {
        let bytes = mapping
            .range
            .pages
            .checked_mul(PAGE)
            .ok_or(llfree::Error::Address)?;
        if bytes == 0
            || mapping.range.addr & 0xfff != 0
            || mapping.range.addr.checked_add(bytes - 1).is_none()
            || mapping.physical_addr.is_some_and(|p| {
                p.get() & 0xfff != 0
                    || p.get()
                        .checked_add(bytes)
                        .is_none_or(|end| end > (1usize << 56))
            })
        {
            return Err(llfree::Error::Address);
        }
        self.check_free(root, mapping.range.addr, mapping.range.pages)?;
        let mut table = 0;
        for i in 0..mapping.range.pages {
            let address = mapping.range.addr + i * PAGE;
            let index = (address >> 12) & 511;
            if i == 0 || index == 0 {
                (table, _) = self.leaf(root, address)?;
            }
            let (physical, owned) = match mapping.physical_addr {
                Some(p) => (p.get() + i * PAGE, 0),
                None => (self.new_page()?, OWNED),
            };
            self.write_pte(
                PTE_L2_SLOT,
                table,
                index,
                (physical >> 2) | flags_new(&mapping.flags) | owned,
            );
        }
        if mapping.flags[4] != b'-' || root.page() == Window::current_root().page() {
            sfence_vma(None, (mapping.flags[4] == b'-').then(|| root.asid()));
        }
        Ok(())
    }

    fn release_table(&mut self, physical: usize, level: usize) -> Result<(), llfree::Error> {
        let slot = match level {
            2 => PTE_L0_SLOT,
            1 => PTE_L1_SLOT,
            _ => PTE_L2_SLOT,
        };
        for i in 0..512 {
            let pte = self.read_pte(slot, physical, i);
            if pte & OWNED == 0 {
                continue;
            }
            if pte & 14 == 0 && level > 0 {
                self.release_table(pte_page(pte), level - 1)?;
            } else {
                self.hart().free(pte_page(pte))?;
            }
        }
        self.hart().free(physical)
    }

    /// Reclaim an inactive root and its owned pages/tables. Shared ELF stems,
    /// module tables and supervisor mappings are borrowed entries.
    /// The root's translations must be invalidated before reclamation.
    pub fn destroy(&mut self, root: Root) -> Result<(), llfree::Error> {
        self.release_table(root.page(), 2)
    }

    /// Copy only borrowed supervisor mappings; metadata L2 is constructed anew.
    pub fn supervisor_mappings(&mut self, caller: Root, root: Root) -> Result<(), llfree::Error> {
        let old = self
            .root_branch(caller, SUPERVISOR_INDEX)
            .ok_or(llfree::Error::Address)?;
        let new = self.hart().page()?;
        self.write_pte(
            PTE_L0_SLOT,
            root.page(),
            usize::from(SUPERVISOR_INDEX),
            branch(new) | OWNED,
        );
        self.alias(PTE_L1_SLOT, old, true);
        self.alias(PAGE_SLOT, new, true);
        let src = unsafe { UnsafeCell::raw_get(&raw const (*self.base).pte_l1).cast::<usize>() };
        let dst = unsafe { UnsafeCell::raw_get(&raw const (*self.base).page).cast::<usize>() };
        for i in 0..512 {
            let pte = if i == 1 {
                0
            } else {
                unsafe { src.add(i).read_volatile() & !OWNED }
            };
            unsafe { dst.add(i).write_volatile(pte) };
        }
        Ok(())
    }

    /// Construct a fresh supervisor branch with a private per-hart window and stack.
    pub fn hart_mappings(
        &mut self,
        caller: Root,
        root: Root,
        hart_id: usize,
    ) -> Result<(), llfree::Error> {
        let old = self
            .root_branch(caller, SUPERVISOR_INDEX)
            .ok_or(llfree::Error::Address)?;
        let l1 = self.hart().page()?;
        self.write_pte(
            PTE_L0_SLOT,
            root.page(),
            usize::from(SUPERVISOR_INDEX),
            branch(l1) | OWNED,
        );
        let window_l2 = self.new_page()?;
        let hart_mmu = self.new_page()?;
        let current = self.hart();
        let new_hart = Hart::new(
            hart_id,
            current.allocator,
            current.base_addr,
            current.dma_allocator,
            current.dma_base_addr,
        );
        let dst = unsafe { UnsafeCell::raw_get(&raw const (*self.base).page).cast::<Hart>() };
        unsafe { dst.write(new_hart) };
        let stack = self.new_page()?;
        self.write_pte(
            PTE_L2_SLOT,
            window_l2,
            0,
            (window_l2 >> 2) | flags_new(b"rw---"),
        );
        self.write_pte(
            PTE_L2_SLOT,
            window_l2,
            HART_SLOT,
            (hart_mmu >> 2) | flags_new(b"rw---") | OWNED,
        );
        self.write_pte(
            PTE_L2_SLOT,
            window_l2,
            STACK_SLOT,
            (stack >> 2) | flags_new(b"rw---") | OWNED,
        );
        self.alias(PTE_L1_SLOT, old, true);
        self.alias(PAGE_SLOT, l1, true);
        let src = unsafe { UnsafeCell::raw_get(&raw const (*self.base).pte_l1).cast::<usize>() };
        let dst = unsafe { UnsafeCell::raw_get(&raw const (*self.base).page).cast::<usize>() };
        for i in 0..512 {
            let pte = match i {
                0 => branch(window_l2) | 32 | OWNED,
                1 => 0,
                _ => unsafe { src.add(i).read_volatile() & !OWNED },
            };
            unsafe { dst.add(i).write_volatile(pte) };
        }
        Ok(())
    }

    fn empty_table(&mut self, slot: usize, physical: usize) -> bool {
        (0..512).all(|i| self.read_pte(slot, physical, i) == 0)
    }

    fn unmap_stem_leaf(
        &mut self,
        stem: usize,
        asid: Option<u16>,
        address: usize,
    ) -> Result<bool, llfree::Error> {
        let si = (address >> 21) & 511;
        let pte = self.read_pte(PTE_L1_SLOT, stem, si);
        if pte & 1 != 0 && pte & 14 == 0 {
            let leaf = pte_page(pte);
            let index = (address >> 12) & 511;
            let entry = self.read_pte(PTE_L2_SLOT, leaf, index);
            if entry != 0 {
                self.write_pte(PTE_L2_SLOT, leaf, index, 0);
                sfence_vma(Some(address), asid);
                if entry & OWNED != 0 {
                    self.hart().free(pte_page(entry))?;
                }
            }
            if self.empty_table(PTE_L2_SLOT, leaf) {
                self.write_pte(PTE_L1_SLOT, stem, si, 0);
                sfence_vma(None, asid);
                self.hart().free(leaf)?;
                return Ok(true);
            }
            return Ok(false);
        }
        Ok(pte == 0)
    }

    pub fn unmap_stem(
        &mut self,
        stem: usize,
        asid: Option<u16>,
        address: usize,
    ) -> Result<(), llfree::Error> {
        let _ = self.unmap_stem_leaf(stem, asid, address)?;
        Ok(())
    }

    pub fn unmap(&mut self, root: Root, address: usize) -> Result<(), llfree::Error> {
        let ri = addr_to_index(address);
        let Some(stem) = self.root_branch(root, ri) else {
            return Ok(());
        };
        // Also reclaim empty intermediate tables left by a failed allocation.
        if self.unmap_stem_leaf(stem, Some(root.asid()), address)?
            && self.empty_table(PTE_L1_SLOT, stem)
        {
            self.write_pte(PTE_L0_SLOT, root.page(), usize::from(ri), 0);
            sfence_vma(None, Some(root.asid()));
            self.hart().free(stem)?;
        }
        Ok(())
    }
}

#[inline(always)]
fn sfence_vma(addr: Option<usize>, asid: Option<u16>) {
    use core::arch;

    // Keep a compiler memory barrier as well as the hardware fence: page-table
    // stores must be visible before subsequent address translations.
    unsafe {
        match (addr, asid) {
            (None, None) => {
                arch::asm!("sfence.vma zero, zero", options(nostack))
            }
            (None, Some(asid)) => {
                arch::asm!("sfence.vma zero, a1", in("a1") usize::from(asid), options(nostack))
            }
            (Some(addr), None) => {
                arch::asm!("sfence.vma a0, zero", in("a0") addr, options(nostack))
            }
            (Some(addr), Some(asid)) => {
                arch::asm!(
                    "sfence.vma a0, a1",
                    in("a0") addr,
                    in("a1") usize::from(asid),
                    options(nostack),
                )
            }
        }
    }
}
