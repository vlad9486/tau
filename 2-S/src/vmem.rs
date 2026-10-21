// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{
    arch,
    cell::UnsafeCell,
    fmt, hint,
    mem::{self, MaybeUninit},
    num::NonZeroUsize,
};

use super::{
    csr, llfree,
    module::{ModuleTables, PreparedModule, TABLE_PAGES},
};

pub const SV39: usize = 8;
pub const PAGE: usize = 4096;
pub const SCHEDULER: usize = 0xffff_ffc0_0040_0000;
pub const ALLOCATOR: usize = 0xffff_ffc0_006e_0000;
pub const MODULE_CONTEXT: usize = 0xffff_ffc0_0021_0000;
pub const MODULE: usize = 0xffff_ffc0_0022_0000;
pub const SUPERVISOR_INDEX: usize = 256;
// Sv39 RSW: this entry owns an allocator page (data or a child table).
// Borrowed entries stop reclamation, including whole shared branches.
pub const OWNED: usize = 1 << 8;

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
        let v = (SV39 << 60) | ((asid as usize) << 44) | (ptr >> 12);
        // Safety: `v` is never zero by construction
        Self(unsafe { NonZeroUsize::new_unchecked(v) })
    }

    pub fn asid(self) -> u16 {
        ((self.0.get() >> 44) & 0xffff) as u16
    }

    pub fn page(self) -> usize {
        (self.0.get() & ((1 << 44) - 1)) << 12
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

#[repr(C)]
pub struct Hart {
    mask: u16,
    flush: bool,
    next_asid: usize,
    pub hart_id: usize,
}

impl fmt::Display for Hart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Hart { mask, flush, .. } = self;
        writeln!(f, "ASID mask=0x{mask:04x} force_flush={flush}")
    }
}

/// Linker-provided layout of the 2 MiB hart-local window. The first page maps
/// itself, slot 6 holds MMU state, and slot 511 is the supervisor stack.
/// Reserved bytes may be unmapped or temporarily aliased; never borrow or copy
/// the whole structure. Access only mapped fields or use `WindowAccess`.
#[repr(C)]
#[rustfmt::skip]
pub struct Window {
    table: UnsafeCell<[usize; 512]>,                              // 1 page  (slot 0: recursive L2)
    pte: UnsafeCell<[usize; 512]>,                                // 1 page  (slot 1: page-table walk)
    page: UnsafeCell<MaybeUninit<Page>>,                          // 1 page  (slot 2: zeroing / segment copy)
    _reserved3: UnsafeCell<MaybeUninit<Page>>,                    // 1 page  (slot 3: reserved)
    descriptor: UnsafeCell<MaybeUninit<Padded<PreparedModule>>>,  // 1 page  (slot 4: foreign module tables page 0 / descriptor)
    wait: UnsafeCell<MaybeUninit<Page>>,                          // 1 page  (slot 5: WaitRecord / wait slot)
    hart: UnsafeCell<MaybeUninit<Padded<Hart>>>,                  // 1 page  (slot 6: permanent per-hart MMU)
    module: UnsafeCell<MaybeUninit<ModuleTables>>,                // 5 pages (slots 7..12: foreign ModuleTables)
    _reserved: UnsafeCell<MaybeUninit<[Page; 499]>>,              // unused window slots
    stack: UnsafeCell<MaybeUninit<Page>>,                         // 1 page  (slot 511: supervisor stack)
}

const PTE_SLOT: usize = mem::offset_of!(Window, pte) / PAGE;
const PAGE_SLOT: usize = mem::offset_of!(Window, page) / PAGE;
const DESCRIPTOR_SLOT: usize = mem::offset_of!(Window, descriptor) / PAGE;
const WAIT_SLOT: usize = mem::offset_of!(Window, wait) / PAGE;
pub const HART_SLOT: usize = mem::offset_of!(Window, hart) / PAGE;
const MODULE_SLOT: usize = mem::offset_of!(Window, module) / PAGE;
pub const STACK_SLOT: usize = mem::offset_of!(Window, stack) / PAGE;

const _: () = {
    assert!(PTE_SLOT == 1);
    assert!(PAGE_SLOT == 2);
    assert!(DESCRIPTOR_SLOT == 4);
    assert!(WAIT_SLOT == 5);
    assert!(HART_SLOT == 6);
    assert!(MODULE_SLOT == 7);
    assert!(STACK_SLOT == 511);
    assert!(size_of::<Window>() == 512 * PAGE);
};

/// Exclusive access to a hart's page table, MMU state and temporary aliases.
/// The handle retains the caller's pointer without borrowing the sparse window
/// or its active stack.
pub struct WindowAccess {
    base: *const Window,
}

impl Window {
    pub fn current_root() -> Root {
        Root(unsafe { NonZeroUsize::new_unchecked(csr::r!("satp")) })
    }
}

impl WindowAccess {
    /// # Safety
    /// `base` must be aligned and have provenance for the full window layout.
    /// Its table and hart fields must be mapped and initialized (zeroed hart
    /// storage is valid). This handle must have exclusive access to those fields
    /// and the temporary aliases. Every root used while the handle is live must
    /// retain the same window backing.
    pub unsafe fn new(base: *const Window) -> Self {
        Self { base }
    }

    pub fn init(&mut self, hart_id: usize) -> Root {
        let original = Window::current_root();
        let probe = Root::sv39(original.page(), u16::MAX);
        unsafe {
            arch::asm!("csrw satp, {0}", in(reg) probe.0.get(), options(nostack));
        }
        let mask = Window::current_root().asid();
        unsafe {
            arch::asm!("csrw satp, {0}", in(reg) original.0.get(), options(nostack));
        }
        sfence_vma(None, None);
        *self.hart_mut() = Hart {
            mask,
            flush: mask == 0,
            next_asid: 1,
            hart_id,
        };

        original
    }

    /// Two different roots sharing ASID zero always need a fence. Check the roots
    /// first so an untagged MMU need not translate/load the separate hart page on
    /// every switch. Keep the hart policy for tagged switches and forced flushing.
    #[inline(always)]
    pub fn switch_flush(&self, source: Root, destination: Root) -> bool {
        if (source.asid() | destination.asid()) == 0 || self.hart().flush {
            hint::cold_path();
            true
        } else {
            false
        }
    }

    /// Allocate only after construction succeeds, so failed attempts consume no tag.
    pub fn allocate_tag(&mut self) -> Option<u16> {
        let hart = self.hart_mut();
        if hart.mask == 0 {
            Some(0)
        } else if hart.next_asid <= hart.mask as usize {
            let tag = hart.next_asid as u16;
            hart.next_asid += 1;
            Some(tag)
        } else {
            None
        }
    }

    pub fn hart(&self) -> &Hart {
        unsafe {
            &(&*UnsafeCell::raw_get(&raw const (*self.base).hart))
                .assume_init_ref()
                .inner
        }
    }

    pub fn hart_mut(&mut self) -> &mut Hart {
        unsafe {
            &mut (&mut *UnsafeCell::raw_get(&raw const (*self.base).hart))
                .assume_init_mut()
                .inner
        }
    }

    fn alias(&mut self, slot: usize, physical: usize) {
        debug_assert!(slot > 0 && slot < 511 && slot != HART_SLOT && physical & 0xfff == 0);
        let pte = (physical >> 2) | flags_new(b"rw---");
        let target = self.base.addr() + slot * PAGE;
        let table = unsafe { UnsafeCell::raw_get(&raw const (*self.base).table) };
        let cell = unsafe { table.cast::<usize>().add(slot) };
        if unsafe { cell.read_volatile() } != pte {
            unsafe { cell.write_volatile(pte) };
            sfence_vma(Some(target), None);
        }
    }

    fn read_pte(&mut self, table: usize, index: usize) -> usize {
        debug_assert!(index < 512);
        self.alias(PTE_SLOT, table);
        let ptr = unsafe { UnsafeCell::raw_get(&raw const (*self.base).pte) };
        let cell = unsafe { ptr.cast::<usize>().add(index) };
        unsafe { cell.read_volatile() }
    }

    fn write_pte(&mut self, table: usize, index: usize, pte: usize) {
        debug_assert!(index < 512);
        self.alias(PTE_SLOT, table);
        let ptr = unsafe { UnsafeCell::raw_get(&raw const (*self.base).pte) };
        let cell = unsafe { ptr.cast::<usize>().add(index) };
        unsafe { cell.write_volatile(pte) };
    }

    pub fn new_page(
        &mut self,
        page: impl Fn() -> Result<usize, llfree::Error>,
    ) -> Result<usize, llfree::Error> {
        let physical = page()?;
        self.page_mut(physical).fill(0);
        Ok(physical)
    }

    pub fn create(
        &mut self,
        asid: u16,
        page: impl Fn() -> Result<usize, llfree::Error>,
    ) -> Result<Root, llfree::Error> {
        Ok(Root::sv39(self.new_page(page)?, asid))
    }

    pub fn page_mut(&mut self, physical: usize) -> &mut [u8; PAGE] {
        self.alias(PAGE_SLOT, physical);
        let ptr = unsafe { UnsafeCell::raw_get(&raw const (*self.base).page) };
        unsafe { &mut (*ptr).assume_init_mut().0 }
    }

    /// Read the prepared module header from page 0 of foreign module tables.
    pub fn read_descriptor(&mut self, physical: usize) -> PreparedModule {
        self.alias(DESCRIPTOR_SLOT, physical);
        let ptr = unsafe { UnsafeCell::raw_get(&raw const (*self.base).descriptor) };
        unsafe { (*ptr).assume_init_ref().inner }
    }

    pub fn wait_mut(&mut self, physical: usize) -> &mut [u8; PAGE] {
        self.alias(WAIT_SLOT, physical);
        let ptr = unsafe { UnsafeCell::raw_get(&raw const (*self.base).wait) };
        unsafe { &mut (*ptr).assume_init_mut().0 }
    }

    pub fn map_module_tables(&mut self, physical: usize) -> &mut ModuleTables {
        for i in 0..TABLE_PAGES {
            self.alias(MODULE_SLOT + i, physical + i * PAGE);
        }
        let ptr = unsafe { UnsafeCell::raw_get(&raw const (*self.base).module) };
        unsafe { (&mut *ptr).assume_init_mut() }
    }

    pub fn root_branch(&mut self, root: Root, index: usize) -> Option<usize> {
        let pte = self.read_pte(root.page(), index);
        (pte & 1 != 0 && pte & 14 == 0).then(|| pte_page(pte))
    }
    pub fn attach(&mut self, root: Root, index: usize, stem: usize) {
        debug_assert!(self.read_pte(root.page(), index) == 0);
        self.write_pte(root.page(), index, branch(stem));
    }

    fn leaf_stem(
        &mut self,
        stem: usize,
        address: usize,
        page: impl Fn() -> Result<usize, llfree::Error>,
    ) -> Result<(usize, usize), llfree::Error> {
        let index = (address >> 21) & 511;
        let pte = self.read_pte(stem, index);
        let table = if pte == 0 {
            let next = self.new_page(&page)?;
            self.write_pte(stem, index, branch(next) | OWNED);
            next
        } else if pte & 1 != 0 && pte & 14 == 0 {
            pte_page(pte)
        } else {
            return Err(llfree::Error::Address);
        };
        Ok((table, (address >> 12) & 511))
    }

    fn leaf(
        &mut self,
        root: Root,
        address: usize,
        page: impl Fn() -> Result<usize, llfree::Error>,
    ) -> Result<(usize, usize), llfree::Error> {
        let ri = (address >> 30) & 511;
        let pte = self.read_pte(root.page(), ri);
        let stem = if pte == 0 {
            let next = self.new_page(&page)?;
            self.write_pte(root.page(), ri, branch(next) | OWNED);
            next
        } else if pte & 1 != 0 && pte & 14 == 0 {
            pte_page(pte)
        } else {
            return Err(llfree::Error::Address);
        };
        self.leaf_stem(stem, address, page)
    }

    pub fn resolve_stem(&mut self, stem: usize, address: usize) -> Option<(usize, usize)> {
        let mut table = stem;
        for shift in [21, 12] {
            let pte = self.read_pte(table, (address >> shift) & 511);
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
        for shift in [30, 21, 12] {
            let pte = self.read_pte(table, (address >> shift) & 511);
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
            let pte = self.read_pte(stem, si);
            if pte != 0 {
                if pte & 1 == 0 || pte & 14 != 0 {
                    return Err(llfree::Error::Address);
                }
                let leaf = pte_page(pte);
                for offset in 0..chunk {
                    if self.read_pte(leaf, index + offset) != 0 {
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
            let pte = self.read_pte(root.page(), ri);
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

    pub fn range_owned(&mut self, root: Root, mut address: usize, mut pages: usize) -> bool {
        while pages != 0 {
            let ri = (address >> 30) & 511;
            let stem_offset = (address >> 12) & ((1 << 18) - 1);
            let stem_chunk = pages.min((1 << 18) - stem_offset);
            if let Some(stem) = self.root_branch(root, ri) {
                let mut rem = stem_chunk;
                let mut addr = address;
                while rem != 0 {
                    let si = (addr >> 21) & 511;
                    let index = (addr >> 12) & 511;
                    let chunk = rem.min(512 - index);
                    let pte = self.read_pte(stem, si);
                    if pte & 1 != 0 && pte & 14 == 0 {
                        let leaf = pte_page(pte);
                        for offset in 0..chunk {
                            let entry = self.read_pte(leaf, index + offset);
                            if entry & 1 != 0 && entry & 14 != 0 && entry & OWNED == 0 {
                                return false;
                            }
                        }
                    }
                    addr = addr.wrapping_add(chunk * PAGE);
                    rem -= chunk;
                }
            }
            address = address.wrapping_add(stem_chunk * PAGE);
            pages -= stem_chunk;
        }
        true
    }

    pub fn map_stem(
        &mut self,
        stem: usize,
        mapping: Mapping,
        page: impl Fn() -> Result<usize, llfree::Error>,
    ) -> Result<(), llfree::Error> {
        let bytes = mapping
            .virtual_pages
            .checked_mul(PAGE)
            .ok_or(llfree::Error::Address)?;
        if bytes == 0
            || mapping.addr_virtual & 0xfff != 0
            || mapping.addr_virtual.checked_add(bytes - 1).is_none()
            || mapping.physical_pages > mapping.virtual_pages
            || mapping
                .addr_physical
                .is_some_and(|p| p.get() & 0xfff != 0 || p.get().checked_add(bytes).is_none())
        {
            return Err(llfree::Error::Address);
        }
        self.check_free_stem(stem, mapping.addr_virtual, mapping.virtual_pages)?;
        let mut table = 0;
        for i in 0..mapping.virtual_pages {
            let address = mapping.addr_virtual + i * PAGE;
            let index = (address >> 12) & 511;
            if i == 0 || index == 0 {
                (table, _) = self.leaf_stem(stem, address, &page)?;
            }
            let (physical, owned) = match mapping.addr_physical {
                Some(p) if i < mapping.physical_pages => (p.get() + i * PAGE, 0),
                _ => (self.new_page(&page)?, OWNED),
            };
            self.write_pte(
                table,
                index,
                (physical >> 2) | flags_new(&mapping.flags) | owned,
            );
        }
        Ok(())
    }

    pub fn map(
        &mut self,
        root: Root,
        mapping: Mapping,
        page: impl Fn() -> Result<usize, llfree::Error>,
    ) -> Result<(), llfree::Error> {
        let bytes = mapping
            .virtual_pages
            .checked_mul(PAGE)
            .ok_or(llfree::Error::Address)?;
        if bytes == 0
            || mapping.addr_virtual & 0xfff != 0
            || mapping.addr_virtual.checked_add(bytes - 1).is_none()
            || mapping.physical_pages > mapping.virtual_pages
            || mapping
                .addr_physical
                .is_some_and(|p| p.get() & 0xfff != 0 || p.get().checked_add(bytes).is_none())
        {
            return Err(llfree::Error::Address);
        }
        self.check_free(root, mapping.addr_virtual, mapping.virtual_pages)?;
        let mut table = 0;
        for i in 0..mapping.virtual_pages {
            let address = mapping.addr_virtual + i * PAGE;
            let index = (address >> 12) & 511;
            if i == 0 || index == 0 {
                (table, _) = self.leaf(root, address, &page)?;
            }
            let (physical, owned) = match mapping.addr_physical {
                Some(p) if i < mapping.physical_pages => (p.get() + i * PAGE, 0),
                _ => (self.new_page(&page)?, OWNED),
            };
            self.write_pte(
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

    fn release_table(
        &mut self,
        physical: usize,
        level: usize,
        free: &impl Fn(usize) -> Result<(), llfree::Error>,
    ) -> Result<(), llfree::Error> {
        for i in 0..512 {
            let pte = self.read_pte(physical, i);
            if pte & OWNED == 0 {
                continue;
            }
            if pte & 14 == 0 && level > 0 {
                self.release_table(pte_page(pte), level - 1, free)?;
            } else {
                free(pte_page(pte))?;
            }
        }
        free(physical)
    }

    /// Reclaim an inactive root and its owned pages/tables. Shared ELF stems,
    /// module tables and supervisor mappings are borrowed entries.
    /// The root's translations must be invalidated before reclamation.
    pub fn destroy(
        &mut self,
        root: Root,
        free: impl Fn(usize) -> Result<(), llfree::Error>,
    ) -> Result<(), llfree::Error> {
        self.release_table(root.page(), 2, &free)
    }

    /// Copy only borrowed supervisor mappings; metadata L2 is constructed anew.
    pub fn supervisor_mappings(
        &mut self,
        caller: Root,
        root: Root,
        page: impl Fn() -> Result<usize, llfree::Error>,
    ) -> Result<(), llfree::Error> {
        let old = self
            .root_branch(caller, SUPERVISOR_INDEX)
            .ok_or(llfree::Error::Address)?;
        let new = page()?;
        self.write_pte(root.page(), SUPERVISOR_INDEX, branch(new) | OWNED);
        self.alias(PTE_SLOT, old);
        self.alias(PAGE_SLOT, new);
        let src = unsafe { UnsafeCell::raw_get(&raw const (*self.base).pte).cast::<usize>() };
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

    fn empty_table(&mut self, physical: usize) -> bool {
        (0..512).all(|i| self.read_pte(physical, i) == 0)
    }

    fn unmap_stem_leaf(
        &mut self,
        stem: usize,
        asid: Option<u16>,
        address: usize,
        free: &impl Fn(usize) -> Result<(), llfree::Error>,
    ) -> Result<bool, llfree::Error> {
        let si = (address >> 21) & 511;
        let pte = self.read_pte(stem, si);
        if pte & 1 != 0 && pte & 14 == 0 {
            let leaf = pte_page(pte);
            let index = (address >> 12) & 511;
            let entry = self.read_pte(leaf, index);
            if entry != 0 {
                self.write_pte(leaf, index, 0);
                if let Some(asid) = asid {
                    sfence_vma(Some(address), Some(asid));
                }
                if entry & OWNED != 0 {
                    free(pte_page(entry))?;
                }
            }
            if self.empty_table(leaf) {
                self.write_pte(stem, si, 0);
                if let Some(asid) = asid {
                    sfence_vma(None, Some(asid));
                }
                free(leaf)?;
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
        free: impl Fn(usize) -> Result<(), llfree::Error>,
    ) -> Result<(), llfree::Error> {
        let _ = self.unmap_stem_leaf(stem, asid, address, &free)?;
        Ok(())
    }

    pub fn unmap(
        &mut self,
        root: Root,
        address: usize,
        free: impl Fn(usize) -> Result<(), llfree::Error>,
    ) -> Result<(), llfree::Error> {
        let ri = (address >> 30) & 511;
        let Some(stem) = self.root_branch(root, ri) else {
            return Ok(());
        };
        // Also reclaim empty intermediate tables left by a failed allocation.
        if self.unmap_stem_leaf(stem, Some(root.asid()), address, &free)? && self.empty_table(stem)
        {
            self.write_pte(root.page(), ri, 0);
            sfence_vma(None, Some(root.asid()));
            free(stem)?;
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
                arch::asm!("sfence.vma zero, a1", in("a1") asid, options(nostack))
            }
            (Some(addr), None) => {
                arch::asm!("sfence.vma a0, zero", in("a0") addr, options(nostack))
            }
            (Some(addr), Some(asid)) => {
                arch::asm!("sfence.vma a0, a1", in("a0") addr, in("a1") asid, options(nostack))
            }
        }
    }
}
