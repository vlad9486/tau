// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{num::NonZeroUsize, ptr};

use super::{asm, cpu, llfree};

pub const SV39: usize = 8;
pub const PAGE: usize = 4096;
pub const THREAD: usize = 0xffff_ffc0_0020_0000;
pub const MODULE_CONTEXT: usize = 0xffff_ffc0_0021_0000;
pub const MODULE: usize = 0xffff_ffc0_0022_0000;
pub const SUPERVISOR_INDEX: usize = 256;
// Sv39 RSW: this entry owns an allocator page (data or a child table).
// Borrowed entries stop reclamation, including whole shared branches.
const OWNED: usize = 1 << 8;

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

/// The first page maps itself. Other pages in the 2 MiB window are temporary
/// aliases, not Rust-owned fields. Slot 511 is the supervisor stack: never alias it.
#[repr(C)]
pub struct Window {
    table: [usize; 512],
}

impl Window {
    pub fn current_root() -> Root {
        Root(unsafe { NonZeroUsize::new_unchecked(cpu::csrr!("satp")) })
    }

    fn alias(&mut self, slot: usize, physical: usize) -> *mut u8 {
        debug_assert!(slot > 0 && slot < 511 && physical & 0xfff == 0);
        let pte = (physical >> 2) | flags_new(b"rw---");
        let target = ptr::from_mut(self).cast::<u8>().wrapping_add(slot * PAGE);
        let cell = unsafe { self.table.get_unchecked_mut(slot) };
        if *cell != pte {
            unsafe { ptr::from_mut(cell).write_volatile(pte) };
            asm::sfence_vma(Some(target.addr()), None);
        }
        target
    }

    pub fn read_pte(&mut self, table: usize, index: usize) -> usize {
        debug_assert!(index < 512);
        unsafe {
            self.alias(2, table)
                .cast::<usize>()
                .add(index)
                .read_volatile()
        }
    }

    pub fn write_pte(&mut self, table: usize, index: usize, pte: usize) {
        debug_assert!(index < 512);
        unsafe {
            self.alias(2, table)
                .cast::<usize>()
                .add(index)
                .write_volatile(pte)
        };
    }

    fn new_page(
        &mut self,
        page: impl Fn() -> Result<usize, llfree::Error>,
    ) -> Result<usize, llfree::Error> {
        let physical = page()?;
        unsafe { self.page_pointer(physical).write_bytes(0, PAGE) };
        Ok(physical)
    }

    pub fn create(
        &mut self,
        asid: u16,
        page: impl Fn() -> Result<usize, llfree::Error>,
    ) -> Result<Root, llfree::Error> {
        Ok(Root::sv39(self.new_page(page)?, asid))
    }

    /// An alias for a contiguous allocator block. Only construction code may
    /// use this pointer; it must not survive another alias of slots 16..510.
    pub fn block(&mut self, physical: usize, pages: usize) -> *mut u8 {
        debug_assert!(pages > 0 && pages <= 495);
        for i in 0..pages {
            self.alias(16 + i, physical + i * PAGE);
        }
        ptr::from_mut(self).cast::<u8>().wrapping_add(16 * PAGE)
    }

    pub fn page_pointer(&mut self, physical: usize) -> *mut u8 {
        self.alias(3, physical)
    }

    /// Alias a root-local range whose backing pages need not be contiguous.
    /// The pointer expires when slots 16..510 are aliased again.
    pub fn mapped_block(&mut self, root: Root, address: usize, pages: usize) -> Option<*mut u8> {
        debug_assert!(pages > 0 && pages <= 495 && address & 0xfff == 0);
        for i in 0..pages {
            let (physical, _) = self.resolve(root, address + i * PAGE)?;
            self.alias(16 + i, physical);
        }
        Some(ptr::from_mut(self).cast::<u8>().wrapping_add(16 * PAGE))
    }

    pub fn root_branch(&mut self, root: Root, index: usize) -> Option<usize> {
        let pte = self.read_pte(root.page(), index);
        (pte & 1 != 0 && pte & 14 == 0).then(|| pte_page(pte))
    }
    pub fn attach(&mut self, root: Root, index: usize, stem: usize) {
        debug_assert!(self.read_pte(root.page(), index) == 0);
        self.write_pte(root.page(), index, branch(stem));
    }

    fn leaf(
        &mut self,
        root: Root,
        address: usize,
        page: impl Fn() -> Result<usize, llfree::Error>,
    ) -> Result<(usize, usize), llfree::Error> {
        let mut table = root.page();
        for shift in [30, 21] {
            let index = (address >> shift) & 511;
            let pte = self.read_pte(table, index);
            table = if pte == 0 {
                let next = self.new_page(&page)?;
                self.write_pte(table, index, branch(next) | OWNED);
                next
            } else if pte & 1 != 0 && pte & 14 == 0 {
                pte_page(pte)
            } else {
                return Err(llfree::Error::Address);
            };
        }
        Ok((table, (address >> 12) & 511))
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
        for i in 0..mapping.virtual_pages {
            let address = mapping.addr_virtual + i * PAGE;
            let (table, index) = self.leaf(root, address, &page)?;
            if self.read_pte(table, index) != 0 {
                return Err(llfree::Error::InvalidArgument);
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
        asm::sfence_vma(
            None,
            if mapping.flags[4] == b'-' {
                Some(root.asid())
            } else {
                None
            },
        );
        Ok(())
    }

    /// Read userspace through supervisor aliases, without enabling SUM. The
    /// caller validates ranges before copying; unmapped/non-readable pages fail.
    pub fn copy_user(
        &mut self,
        root: Root,
        mut address: usize,
        destination: &mut [u8],
    ) -> Result<(), llfree::Error> {
        if !user_range(address, destination.len()) {
            return Err(llfree::Error::Address);
        }
        let mut done = 0;
        while done < destination.len() {
            let (physical, flags) = self.resolve(root, address).ok_or(llfree::Error::Address)?;
            if flags & 18 != 18 {
                return Err(llfree::Error::Address);
            }
            let offset = physical & 0xfff;
            let count = (PAGE - offset).min(destination.len() - done);
            let source = self.alias(4, physical & !0xfff);
            unsafe {
                ptr::copy_nonoverlapping(
                    source.add(offset),
                    destination.as_mut_ptr().add(done),
                    count,
                )
            };
            done += count;
            address += count;
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
    /// module tables, thread pages and supervisor mappings are borrowed entries.
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
        let new = self.new_page(page)?;
        self.write_pte(root.page(), SUPERVISOR_INDEX, branch(new) | OWNED);
        for i in 0..512 {
            if i != 1 {
                let pte = self.read_pte(old, i);
                self.write_pte(new, i, pte & !OWNED);
            }
        }
        Ok(())
    }

    fn empty_table(&mut self, physical: usize) -> bool {
        (0..512).all(|i| self.read_pte(physical, i) == 0)
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
        let si = (address >> 21) & 511;
        let pte = self.read_pte(stem, si);
        if pte & 1 != 0 && pte & 14 == 0 {
            let leaf = pte_page(pte);
            let index = (address >> 12) & 511;
            let entry = self.read_pte(leaf, index);
            self.write_pte(leaf, index, 0);
            asm::sfence_vma(Some(address), Some(root.asid()));
            if entry & OWNED != 0 {
                free(pte_page(entry))?;
            }
            if self.empty_table(leaf) {
                self.write_pte(stem, si, 0);
                asm::sfence_vma(None, Some(root.asid()));
                free(leaf)?;
            }
        }
        // Also reclaim empty intermediate tables left by a failed allocation.
        if self.empty_table(stem) {
            self.write_pte(root.page(), ri, 0);
            asm::sfence_vma(None, Some(root.asid()));
            free(stem)?;
        }
        Ok(())
    }
}
