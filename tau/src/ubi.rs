// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{arch, hint, num::NonZeroUsize};

use super::{
    Call, Entry, Event, MemoryZone, ModuleError, ModuleHandle, PageFlags, SpawnError, WaitError,
    WaitSlot,
};

#[inline(always)]
fn ubi<const I: usize, const O: usize>(call: Call, arg: [usize; I]) -> (usize, [usize; O]) {
    const {
        assert!(I <= 5 && O <= 5);
    }

    let mut words = [0; 5];
    words[..I].copy_from_slice(&arg);
    let status;
    unsafe {
        arch::asm!(
            "ecall",
            inout("a0") call.encode() => status,
            inout("a1") words[0],
            inout("a2") words[1],
            inout("a3") words[2],
            inout("a4") words[3],
            inout("a5") words[4],
            clobber_abi("C"),
            options(nostack),
        );
    }
    let mut output = [0; O];
    output.copy_from_slice(&words[..O]);
    (status, output)
}

/// The system interface (User Binary Interface).
pub struct Ubi;

impl Ubi {
    /// Allocate `2^order` contiguous zeroed physical frames from `zone`.
    /// Requires `loader_authority`.
    pub fn alloc_physical(order: u8, zone: MemoryZone) -> Result<usize, AllocError> {
        let (r0, [phys]) = ubi(Call::AllocPhysical { order, zone }, []);
        if let Some(code) = NonZeroUsize::new(r0) {
            Err(unsafe { AllocError::decode(code) })
        } else {
            Ok(phys)
        }
    }

    /// Release `2^order` contiguous physical frames at `physical_addr` back to `zone`.
    /// Requires `loader_authority`.
    pub fn free_physical(
        physical_addr: usize,
        order: u8,
        zone: MemoryZone,
    ) -> Result<(), FreeError> {
        let (r0, []) = ubi(Call::FreePhysical { order, zone }, [physical_addr]);
        if let Some(code) = NonZeroUsize::new(r0) {
            Err(unsafe { FreeError::decode(code) })
        } else {
            Ok(())
        }
    }

    /// Create an empty module template and shared tables, returning its handle.
    /// Requires `loader_authority`.
    pub fn create_module(
        entry: usize,
        stack_pages: usize,
        wait_slots_addr: usize,
        wait_slots_count: usize,
    ) -> Result<ModuleHandle, ModuleError> {
        let (status, [handle]) = ubi(
            Call::CreateModule,
            [entry, stack_pages, wait_slots_addr, wait_slots_count],
        );
        module_status(status)?;
        Ok(ModuleHandle::from_raw(handle))
    }

    /// Parse an ELF image in userspace, stage its segments via zero-copy page
    /// handover, and bind the resulting module to `parent_module_id`.
    #[cfg(feature = "elf")]
    pub fn prepare_module(
        parent_module_id: ModuleHandle,
        image: &[u8],
    ) -> Result<(u16, ModuleHandle), ModuleError> {
        prepare_elf_module(parent_module_id, image)
    }

    /// Attach an existing module instance without copying its ELF or globals.
    /// Each call allocates a fresh parent-local slot. ROOT is valid only as a
    /// parent. The trusted loader must validate that this edge creates no cycle.
    pub fn bind_dependency(
        parent_module_id: ModuleHandle,
        dependency_module_id: ModuleHandle,
    ) -> Result<u16, ModuleError> {
        let (status, [slot]) = ubi(
            Call::BindDependency,
            [parent_module_id.as_raw(), dependency_module_id.as_raw()],
        );
        module_status(status)?;
        Ok(slot as u16)
    }

    pub fn invoke<const I: usize, const O: usize>(
        slot: u16,
        arg: u16,
        msg: [usize; I],
    ) -> Result<[usize; O], NonZeroUsize> {
        let (r0, msg) = ubi(Call::Invoke { slot, arg }, msg);
        if let Some(error) = NonZeroUsize::new(r0) {
            Err(error)
        } else {
            Ok(msg)
        }
    }

    pub fn respond<const O: usize>(code: u16, msg: [usize; O]) -> ! {
        let (_, []) = ubi(Call::Respond { code }, msg);
        unsafe { hint::unreachable_unchecked() }
    }

    pub fn spawn<const I: usize>(entry: Entry, msg: [usize; I]) -> Result<u16, SpawnError> {
        let entry = entry as usize;
        let (status, [id]) = ubi(Call::Spawn { entry }, msg);
        match status {
            0 => Ok(id as u16),
            1 => Err(SpawnError::InvalidEntry),
            3 => Err(SpawnError::NoSlots),
            _ => Err(SpawnError::OutOfMemory),
        }
    }

    pub fn exit() -> ! {
        let (_, []) = ubi(Call::Exit, []);
        unsafe { hint::unreachable_unchecked() }
    }

    pub fn map(
        physical_addr: Option<NonZeroUsize>,
        virtual_addr: usize,
        number_of_pages: usize,
    ) -> Result<(), AllocError> {
        Self::map_module(
            ModuleHandle::ROOT,
            physical_addr,
            virtual_addr,
            number_of_pages,
            PageFlags::RW,
        )
    }

    pub fn map_module(
        target: ModuleHandle,
        physical_addr: Option<NonZeroUsize>,
        virtual_addr: usize,
        number_of_pages: usize,
        flags: PageFlags,
    ) -> Result<(), AllocError> {
        let physical_addr = physical_addr.map(NonZeroUsize::get).unwrap_or_default();
        let (r0, []) = ubi(
            Call::Map,
            [
                target.as_raw(),
                physical_addr,
                virtual_addr,
                number_of_pages,
                usize::from(flags.as_raw()),
            ],
        );
        if let Some(code) = NonZeroUsize::new(r0) {
            Err(unsafe { AllocError::decode(code) })
        } else {
            Ok(())
        }
    }

    pub fn unmap(virtual_addr: usize, number_of_pages: usize) -> Result<(), FreeError> {
        Self::unmap_module(ModuleHandle::ROOT, virtual_addr, number_of_pages)
    }

    pub fn unmap_module(
        target: ModuleHandle,
        virtual_addr: usize,
        number_of_pages: usize,
    ) -> Result<(), FreeError> {
        let (r0, []) = ubi(
            Call::Unmap,
            [target.as_raw(), virtual_addr, number_of_pages],
        );
        if let Some(code) = NonZeroUsize::new(r0) {
            Err(unsafe { FreeError::decode(code) })
        } else {
            Ok(())
        }
    }

    /// Consume readiness or suspend this thread, resuming here in this context.
    /// Shared-state publication/checking still needs module synchronization.
    pub fn wait(slot: *const WaitSlot, deadline: Option<NonZeroUsize>) -> Result<Event, WaitError> {
        let deadline = deadline.map(NonZeroUsize::get).unwrap_or_default();
        let (status, [event]) = ubi(Call::Wait, [slot.addr(), deadline]);
        wait_status(status)?;
        Event::decode(event).map_err(|_| WaitError::Unsupported)
    }

    /// Record coalesced readiness for a waiter in this module. Does not transfer
    /// this thread or execute code in the receiver's context.
    pub fn notify(slot: *const WaitSlot) -> Result<(), WaitError> {
        let (status, []) = ubi(Call::Notify, [slot.addr()]);
        wait_status(status)
    }

    /// Print a diagnostic checkpoint and one machine-word value through the
    /// supervisor's SBI debug console.
    pub fn debug(checkpoint: usize, value: usize) {
        let (_, []) = ubi(Call::Debug, [checkpoint, value]);
    }
}

fn module_status(status: usize) -> Result<(), ModuleError> {
    match status {
        0 => Ok(()),
        1 => Err(ModuleError::AccessDenied),
        3 => Err(ModuleError::OutOfMemory),
        4 => Err(ModuleError::NoSlots),
        5 => Err(ModuleError::InvalidDependency),
        0x8040 => Err(ModuleError::Poisoned),
        _ => Err(ModuleError::InvalidImage),
    }
}

#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocError {
    AlreadyAllocated = 1,
    OutOfMemory = 2,
    AccessDenied = 3,
    InvalidAddress = 4,
}

impl AllocError {
    /// # Safety
    /// The code must be in range of `Self`
    #[inline]
    pub const unsafe fn decode(code: NonZeroUsize) -> Self {
        match code.get() {
            1 => Self::AlreadyAllocated,
            2 => Self::OutOfMemory,
            3 => Self::AccessDenied,
            4 => Self::InvalidAddress,
            _ => unsafe { hint::unreachable_unchecked() },
        }
    }
}

#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeError {
    AlreadyFree = 1,
    AccessDenied = 2,
    InvalidAddress = 3,
}

impl FreeError {
    /// # Safety
    /// The code must be in range of `Self`
    #[inline]
    pub const unsafe fn decode(code: NonZeroUsize) -> Self {
        match code.get() {
            1 => Self::AlreadyFree,
            2 => Self::AccessDenied,
            3 => Self::InvalidAddress,
            _ => unsafe { hint::unreachable_unchecked() },
        }
    }
}

fn wait_status(status: usize) -> Result<(), WaitError> {
    match status {
        0 => Ok(()),
        1 => Err(WaitError::Unsupported),
        2 => Err(WaitError::InvalidWaiter),
        3 => Err(WaitError::AccessDenied),
        4 => Err(WaitError::Busy),
        5 => Err(WaitError::OutOfMemory),
        _ => Err(WaitError::Unsupported),
    }
}

#[cfg(feature = "elf")]
fn prepare_elf_module(
    parent: ModuleHandle,
    bytes: &[u8],
) -> Result<(u16, ModuleHandle), ModuleError> {
    use core::{mem, ptr};
    use elf::{
        ElfBytes,
        abi::{EM_RISCV, ET_EXEC, PF_R, PF_W, PF_X, PT_LOAD},
        endian::LittleEndian,
    };
    use super::{Manifest, MappedRegion, ModuleId};

    const PAGE: usize = 4096;
    const STAGE_VA: usize = 31 << 30;

    fn user_range(address: usize, bytes: usize) -> bool {
        if bytes == 0 {
            return false;
        }
        let Some(last) = address.checked_add(bytes - 1) else {
            return false;
        };
        (address < (1 << 38) && last < (1 << 38))
            || (address >= 0xffff_ffc0_4000_0000 && last >= address)
    }

    fn word(bytes: &[u8], offset: usize) -> Result<usize, ModuleError> {
        Ok(usize::from_le_bytes(
            bytes
                .get(offset..offset + 8)
                .ok_or(ModuleError::InvalidImage)?
                .try_into()
                .map_err(|_| ModuleError::InvalidImage)?,
        ))
    }

    fn image_range<'a>(
        elf: &ElfBytes<'a, LittleEndian>,
        address: usize,
        length: usize,
    ) -> Result<&'a [u8], ModuleError> {
        let end = address
            .checked_add(length)
            .ok_or(ModuleError::InvalidImage)?;
        for ph in elf
            .segments()
            .ok_or(ModuleError::InvalidImage)?
            .iter()
            .filter(|p| p.p_type == PT_LOAD)
        {
            let start = ph.p_vaddr as usize;
            let limit = start
                .checked_add(ph.p_filesz as usize)
                .ok_or(ModuleError::InvalidImage)?;
            if address >= start && end <= limit {
                let data = elf
                    .segment_data(&ph)
                    .map_err(|_| ModuleError::InvalidImage)?;
                return data
                    .get(address - start..end - start)
                    .ok_or(ModuleError::InvalidImage);
            }
        }
        Err(ModuleError::InvalidImage)
    }

    let elf =
        ElfBytes::<LittleEndian>::minimal_parse(bytes).map_err(|_| ModuleError::InvalidImage)?;
    if elf.ehdr.e_machine != EM_RISCV
        || elf.ehdr.e_type != ET_EXEC
        || elf.ehdr.class != elf::file::Class::ELF64
    {
        return Err(ModuleError::InvalidImage);
    }
    let manifest = image_range(&elf, elf.ehdr.e_entry as usize, size_of::<Manifest>())?;
    let entry = word(manifest, mem::offset_of!(Manifest, entry))?;
    let deps = mem::offset_of!(Manifest, dependencies);
    let dependency_count = word(manifest, deps + 8)?;
    if dependency_count != 0 {
        let dep_bytes = dependency_count
            .checked_mul(size_of::<ModuleId>())
            .ok_or(ModuleError::InvalidImage)?;
        image_range(&elf, word(manifest, deps)?, dep_bytes)?;
    }
    let regions = mem::offset_of!(Manifest, mapped_regions);
    if word(manifest, regions + 8)? != 1 {
        return Err(ModuleError::InvalidImage);
    }
    let stack = image_range(&elf, word(manifest, regions)?, size_of::<MappedRegion>())?;
    let stack_pages = word(stack, mem::offset_of!(MappedRegion, pages))?;
    if stack_pages == 0
        || stack_pages > 256
        || word(stack, mem::offset_of!(MappedRegion, phys_start))? != 0
        || word(stack, mem::offset_of!(MappedRegion, virtual_start))?
            != (stack_pages * PAGE).wrapping_neg()
        || stack.get(mem::offset_of!(MappedRegion, write)) != Some(&1)
    {
        return Err(ModuleError::InvalidImage);
    }

    let segments = elf.segments().ok_or(ModuleError::InvalidImage)?;
    let mut root_index = None;
    let mut executable_entry = false;
    let mut total = 0usize;
    for (seg_idx, ph) in segments.iter().enumerate() {
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }
        let address = ph.p_vaddr as usize;
        let end = address
            .checked_add(ph.p_memsz as usize)
            .ok_or(ModuleError::InvalidImage)?;
        let index = (address >> 30) & 511;
        total = total
            .checked_add(ph.p_memsz as usize)
            .ok_or(ModuleError::InvalidImage)?;
        if total > 64 * 1024 * 1024
            || ph.p_filesz > ph.p_memsz
            || !user_range(address, ph.p_memsz as usize)
            || index == 511
            || index == 256
            || ((end - 1) >> 30) & 511 != index
            || root_index.is_some_and(|old| old != index)
            || ph.p_flags & PF_R == 0
            || ph.p_flags & (PF_W | PF_X) == PF_W | PF_X
            || (ph.p_align > 1
                && (!ph.p_align.is_power_of_two()
                    || ph.p_vaddr % ph.p_align != ph.p_offset % ph.p_align))
        {
            return Err(ModuleError::InvalidImage);
        }
        let first = address & !0xfff;
        let page_end = first + (end - first).div_ceil(PAGE) * PAGE;
        for prev in segments.iter().take(seg_idx) {
            if prev.p_type != PT_LOAD || prev.p_memsz == 0 {
                continue;
            }
            let prev_addr = prev.p_vaddr as usize;
            let prev_end = prev_addr + prev.p_memsz as usize;
            let prev_first = prev_addr & !0xfff;
            let prev_page_end = prev_first + (prev_end - prev_first).div_ceil(PAGE) * PAGE;
            if first < prev_page_end && prev_first < page_end {
                return Err(ModuleError::InvalidImage);
            }
        }
        root_index = Some(index);
        executable_entry |=
            entry >= address && entry < end && ph.p_flags & PF_X != 0 && entry & 1 == 0;
        elf.segment_data(&ph)
            .map_err(|_| ModuleError::InvalidImage)?;
    }
    if !executable_entry {
        return Err(ModuleError::InvalidImage);
    }
    let root_index = root_index.ok_or(ModuleError::InvalidImage)?;
    let wait_base = word(manifest, mem::offset_of!(Manifest, wait_slots))?;
    let wait_count = word(
        manifest,
        mem::offset_of!(Manifest, wait_slots) + size_of::<usize>(),
    )?;
    if wait_count != 0 {
        let wait_bytes = wait_count
            .checked_mul(64)
            .ok_or(ModuleError::InvalidImage)?;
        let wait_end = wait_base
            .checked_add(wait_bytes)
            .ok_or(ModuleError::InvalidImage)?;
        if wait_base == 0
            || wait_base & 63 != 0
            || !user_range(wait_base, wait_bytes)
            || (wait_base >> 30) & 511 != root_index
            || ((wait_end - 1) >> 30) & 511 != root_index
        {
            return Err(ModuleError::InvalidImage);
        }
        for page_addr in (wait_base & !(PAGE - 1)..wait_end).step_by(PAGE) {
            let mut writable = false;
            for ph in segments
                .iter()
                .filter(|p| p.p_type == PT_LOAD && p.p_memsz != 0)
            {
                let first = (ph.p_vaddr as usize) & !0xfff;
                let end = (ph.p_vaddr as usize) + (ph.p_memsz as usize);
                let page_end = first + (end - first).div_ceil(PAGE) * PAGE;
                if page_addr >= first
                    && page_addr < page_end
                    && ph.p_flags & (PF_R | PF_W) == PF_R | PF_W
                {
                    writable = true;
                    break;
                }
            }
            if !writable {
                return Err(ModuleError::InvalidImage);
            }
        }
    }

    let handle = Ubi::create_module(entry, stack_pages, wait_base, wait_count)?;
    for ph in segments
        .iter()
        .filter(|p| p.p_type == PT_LOAD && p.p_memsz != 0)
    {
        let address = ph.p_vaddr as usize;
        let end = address + ph.p_memsz as usize;
        let first = address & !0xfff;
        let pages = (end - first).div_ceil(PAGE);
        let file = elf
            .segment_data(&ph)
            .map_err(|_| ModuleError::InvalidImage)?;
        let mut flags = PageFlags::READ;
        if ph.p_flags & PF_W != 0 {
            flags = flags.union(PageFlags::WRITE);
        }
        if ph.p_flags & PF_X != 0 {
            flags = flags.union(PageFlags::EXEC);
        }
        let file_end = address + file.len();
        for i in 0..pages {
            let page_va = first + i * PAGE;
            let phys =
                Ubi::alloc_physical(0, MemoryZone::Normal).map_err(|_| ModuleError::OutOfMemory)?;
            let copy_start = page_va.max(address);
            let copy_end = (page_va + PAGE).min(file_end);
            if copy_start < copy_end {
                if Ubi::map_module(
                    ModuleHandle::ROOT,
                    NonZeroUsize::new(phys),
                    STAGE_VA,
                    1,
                    PageFlags::RW,
                )
                .is_err()
                {
                    let _ = Ubi::free_physical(phys, 0, MemoryZone::Normal);
                    return Err(ModuleError::OutOfMemory);
                }
                unsafe {
                    ptr::copy_nonoverlapping(
                        file.as_ptr().add(copy_start - address),
                        ptr::with_exposed_provenance_mut::<u8>(STAGE_VA + (copy_start - page_va)),
                        copy_end - copy_start,
                    );
                }
                let _ = Ubi::unmap_module(ModuleHandle::ROOT, STAGE_VA, 1);
            }
            if Ubi::map_module(handle, NonZeroUsize::new(phys), page_va, 1, flags).is_err() {
                let _ = Ubi::free_physical(phys, 0, MemoryZone::Normal);
                return Err(ModuleError::OutOfMemory);
            }
        }
    }
    let slot = Ubi::bind_dependency(parent, handle)?;
    Ok((slot, handle))
}
