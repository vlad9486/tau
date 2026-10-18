// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{hint, num::NonZeroUsize, slice, ptr, mem};

use super::{
    vmem::{Window, WindowAccess, Mapping},
    module::{ModuleContext, ModuleTables},
    scheduler::{Scheduler, Thread},
    waiting::WaitTable,
    llfree, loading, vmem, sbi, layout,
};

pub struct Context {
    pub allocator: llfree::Lower<'static>,
    pub base_addr: usize,
    /// Kept independently of the active satp for later loader/hart startup.
    pub bootstrap_root: vmem::Root,
}

impl Context {
    pub fn block(&self, hart_id: usize, order: usize) -> Result<usize, llfree::Error> {
        let frame = self.allocator.alloc(hart_id, order)?.0;
        Ok(self.base_addr + (frame << 12))
    }

    pub fn page(&self, hart_id: usize) -> Result<usize, llfree::Error> {
        self.block(hart_id, 0)
    }

    pub fn free_block(
        &self,
        hart_id: usize,
        page: usize,
        order: usize,
    ) -> Result<(), llfree::Error> {
        if order > llfree::MAX_ORDER || page & ((1usize << (12 + order)) - 1) != 0 {
            return Err(llfree::Error::Address);
        }
        let frame = page
            .checked_sub(self.base_addr)
            .ok_or(llfree::Error::Address)?
            >> 12;
        let count = 1usize << order;
        if frame
            .checked_add(count)
            .is_none_or(|end| end > self.allocator.frames())
        {
            return Err(llfree::Error::Address);
        }
        self.allocator.put(hart_id, llfree::FrameId(frame), order)
    }

    pub fn free(&self, hart_id: usize, page: usize) -> Result<(), llfree::Error> {
        self.free_block(hart_id, page, 0)
    }
}

#[derive(Debug)]
pub enum InitError {
    Elf(elf::ParseError),
    Allocator(llfree::Error),
    Alloc(tau::AllocError),
    Manifest,
}

/// # Safety
/// Both pointers must denote aligned linker-provided bootstrap addresses which
/// this function maps and initializes with exclusive access. Shared tables must
/// be initialized only once for this module.
pub unsafe fn init(
    window: &mut WindowAccess,
    thread: &Thread,
    module: *mut ModuleContext,
    tables: *mut ModuleTables,
    scheduler: *mut Scheduler,
    context: &Context,
) -> Result<(vmem::Root, usize), InitError> {
    let root = Window::current_root();

    let mapping = Mapping {
        addr_virtual: module.addr(),
        virtual_pages: size_of::<ModuleContext>().div_ceil(0x1000),
        addr_physical: None,
        physical_pages: 0,
        flags: *b"rw---",
    };

    window
        .map(root, mapping, || context.page(thread.hart_id()))
        .map_err(|_| tau::AllocError::OutOfMemory)
        .map_err(InitError::Alloc)?;
    // The mapping is fresh; initialize the complete context value before access.
    unsafe { ModuleContext::init_at(module, root, 0, usize::MAX) };

    // Bootstrap constructs the initial module tables once. Future context
    // creation must reuse their backing pages, not allocate/reset these tables.
    let tables_backing = context
        .block(thread.hart_id(), super::module::TABLE_ORDER)
        .map_err(InitError::Allocator)?;
    let mapping = Mapping {
        addr_virtual: tables.addr(),
        virtual_pages: size_of::<ModuleTables>().div_ceil(0x1000),
        addr_physical: NonZeroUsize::new(tables_backing),
        physical_pages: size_of::<ModuleTables>().div_ceil(0x1000),
        flags: *b"rw---",
    };
    window
        .map(root, mapping, || context.page(thread.hart_id()))
        .map_err(|_| tau::AllocError::OutOfMemory)
        .map_err(InitError::Alloc)?;
    unsafe { ModuleTables::init_at(tables, true, false) };

    let elf_base = context.base_addr + (layout::SYSTEM_OFFSET << 12);
    let data = ptr::from_mut(window).cast::<u8>().with_addr(elf_base);
    let data = unsafe { slice::from_raw_parts(data, layout::SYSTEM_SIZE << 12) };

    use elf::{
        ElfBytes,
        endian::LittleEndian,
        abi::{PT_LOAD, PF_R, PF_W, PF_X},
    };
    let elf = ElfBytes::<LittleEndian>::minimal_parse(data).map_err(InitError::Elf)?;
    let mut manifest_segment = None;
    let mut manifest_range = 0..0;
    if let Some(s) = elf.segments() {
        for ph in s.iter().filter(|s| s.p_type == PT_LOAD) {
            if (ph.p_vaddr..(ph.p_vaddr + ph.p_memsz)).contains(&elf.ehdr.e_entry) {
                // manifest is in this section
                manifest_segment = Some(ph);
                let offset = (elf.ehdr.e_entry - ph.p_vaddr) as usize;
                manifest_range = offset..(offset + size_of::<tau::Manifest>());
            }
            let mut flags = *b"---u-";
            if ph.p_flags & PF_R != 0 {
                flags[0] = b'r';
            }
            if ph.p_flags & PF_W != 0 {
                flags[1] = b'w';
            }
            if ph.p_flags & PF_X != 0 {
                flags[2] = b'x';
            }
            let virtual_pages = ((ph.p_vaddr & 0xfff) + ph.p_memsz).div_ceil(0x1000) as usize;
            let mapping = Mapping {
                addr_virtual: (ph.p_vaddr as usize) & !0xfff,
                virtual_pages,
                addr_physical: None,
                physical_pages: 0,
                flags,
            };

            window
                .map(root, mapping, || context.page(thread.hart_id()))
                .map_err(InitError::Allocator)?;
            // Bootstrap needs owned, zeroed segment backing too. Borrowing the
            // file's last page exposes ELF metadata as nonzero BSS, and lets
            // shared globals overwrite the source image.
            let file = elf.segment_data(&ph).map_err(InitError::Elf)?;
            let mut done = 0;
            while done < file.len() {
                let address = ph.p_vaddr as usize + done;
                let (physical, _) = window.resolve(root, address).ok_or(InitError::Manifest)?;
                let offset = physical & 0xfff;
                let count = (vmem::PAGE - offset).min(file.len() - done);
                unsafe {
                    ptr::copy_nonoverlapping(
                        file.as_ptr().add(done),
                        window.page_mut(physical & !0xfff).as_mut_ptr().add(offset),
                        count,
                    )
                };
                done += count;
            }
        }
    }

    if manifest_range.is_empty() {
        return Err(InitError::Manifest);
    }
    let Some(header) = manifest_segment else {
        return Err(InitError::Manifest);
    };
    let Ok(data) = elf.segment_data(&header) else {
        return Err(InitError::Manifest);
    };
    let Some(manifest) = data.get(manifest_range) else {
        return Err(InitError::Manifest);
    };
    let read_usize = |offset| unsafe {
        let entry = manifest
            .get_unchecked(offset..)
            .get_unchecked(..size_of::<usize>());
        usize::from_ne_bytes(entry.try_into().unwrap_unchecked())
    };

    let sl = mem::offset_of!(tau::Manifest, dependencies);
    let deps_vaddr = read_usize(sl);
    let deps_num = read_usize(sl + size_of::<usize>());

    let offset = deps_vaddr - header.p_vaddr as usize;
    for i in 0..deps_num {
        let range = (offset + i * size_of::<tau::ModuleId>())
            ..(offset + (i + 1) * size_of::<tau::ModuleId>());
        let Some(dep_repr) = data.get(range) else {
            return Err(InitError::Manifest);
        };
        let module_id = unsafe { *dep_repr.as_ptr().cast::<tau::ModuleId>() };
        // TODO: find the module and put in the table
        // tables.insert_dependency(...);
        let _ = module_id;
    }

    let sl = mem::offset_of!(tau::Manifest, mapped_regions);
    let ranges_vaddr = read_usize(sl);
    let ranges_num = read_usize(sl + size_of::<usize>());

    let offset = ranges_vaddr - header.p_vaddr as usize;
    for i in 0..ranges_num {
        let range = (offset + i * size_of::<tau::MappedRegion>())
            ..(offset + (i + 1) * size_of::<tau::MappedRegion>());
        let Some(range_repr) = data.get(range) else {
            return Err(InitError::Manifest);
        };
        let region = unsafe { *range_repr.as_ptr().cast::<tau::MappedRegion>() };
        alloc_pages(
            window,
            thread,
            context,
            region.phys_start,
            region.virtual_start as usize,
            region.pages,
            *b"rw-u-",
        )
        .map_err(InitError::Alloc)?;
    }

    if ranges_num != 1 {
        return Err(InitError::Manifest);
    }
    let stack_offset = ranges_vaddr
        .checked_sub(header.p_vaddr as usize)
        .ok_or(InitError::Manifest)?;
    let stack_bytes = data
        .get(stack_offset..stack_offset + size_of::<tau::MappedRegion>())
        .ok_or(InitError::Manifest)?;
    let stack_pages = usize::from_ne_bytes(
        stack_bytes[mem::offset_of!(tau::MappedRegion, pages)..][..8]
            .try_into()
            .unwrap(),
    );
    let stack_start = usize::from_ne_bytes(
        stack_bytes[mem::offset_of!(tau::MappedRegion, virtual_start)..][..8]
            .try_into()
            .unwrap(),
    );
    if !(1..=256).contains(&stack_pages)
        || stack_start != (stack_pages * vmem::PAGE).wrapping_neg()
        || stack_bytes[..8] != [0; 8]
        || stack_bytes[mem::offset_of!(tau::MappedRegion, write)] != 1
    {
        return Err(InitError::Manifest);
    }
    let sepc = read_usize(mem::offset_of!(tau::Manifest, entry));
    let satp = Window::current_root();
    let stem = (sepc >> 30) & 511;
    let wait_table = WaitTable::prepare(
        window,
        context,
        thread.hart_id(),
        stem,
        read_usize(mem::offset_of!(tau::Manifest, wait_slots)),
        read_usize(mem::offset_of!(tau::Manifest, wait_slots) + size_of::<usize>()),
    )
    .map_err(|_| InitError::Manifest)?;
    // The boot ELF uses the same protected shared-stem rule as prepared ELFs.
    unsafe {
        ptr::addr_of_mut!((*tables).wait_table).write(wait_table);
        ptr::addr_of_mut!((*tables).stem_index).write(stem);
        ptr::addr_of_mut!((*tables).stack_pages).write(stack_pages);
        ModuleContext::init_at(module, satp, sepc, stem);
        Scheduler::initialize(scheduler, window, context, thread.hart_id())
            .map_err(InitError::Alloc)?;
    }
    Ok((satp, sepc))
}

#[inline]
pub fn syscall(
    window: &mut WindowAccess,
    thread: &Thread,
    module: &mut ModuleContext,
    tables: &ModuleTables,
    context: &Context,
    mut msg: [usize; 6],
) -> [usize; 6] {
    use core::fmt::Write;

    match tau::Call::decode(msg[0]) {
        Ok(tau::Call::AllocPhysical { order, zone: _ }) => {
            if !tables.loader_authority {
                msg[0] = tau::AllocError::AccessDenied as usize;
                msg[1] = 0;
            } else {
                match context.block(thread.hart_id(), order as usize) {
                    Ok(phys) => {
                        msg[0] = 0;
                        msg[1] = phys;
                    }
                    Err(_) => {
                        msg[0] = tau::AllocError::OutOfMemory as usize;
                        msg[1] = 0;
                    }
                }
            }
        }
        Ok(tau::Call::FreePhysical { order, zone: _ }) => {
            if !tables.loader_authority {
                msg[0] = tau::FreeError::AccessDenied as usize;
            } else {
                msg[0] = match context.free_block(thread.hart_id(), msg[1], order as usize) {
                    Ok(()) => 0,
                    Err(llfree::Error::Address) => tau::FreeError::InvalidAddress as usize,
                    Err(_) => tau::FreeError::AlreadyFree as usize,
                };
            }
        }
        Ok(tau::Call::CreateModule) => {
            match loading::create(
                window,
                context,
                thread.hart_id(),
                tables,
                msg[1],
                msg[2],
                msg[3],
                msg[4],
            ) {
                Ok(handle) => {
                    msg[0] = 0;
                    msg[1] = handle;
                }
                Err(error) => {
                    msg[0] = error as usize;
                    msg[1] = 0;
                }
            }
        }
        Ok(tau::Call::BindDependency) => {
            match loading::bind(window, context, tables, msg[1], msg[2]) {
                Ok(slot) => {
                    msg[0] = 0;
                    msg[1] = slot as usize;
                }
                Err(error) => {
                    msg[0] = error as usize;
                    msg[1] = 0;
                }
            }
        }
        // These calls need dispatch after all current-context borrows end.
        // The trap boundary handles them before entering here.
        Ok(
            tau::Call::Spawn { .. }
            | tau::Call::Exit
            | tau::Call::Wait
            | tau::Call::Notify
            | tau::Call::Invoke { .. }
            | tau::Call::Respond { .. },
        ) => {
            msg = [tau::WaitError::Unsupported as usize, 0, 0, 0, 0, 0];
        }
        Ok(tau::Call::Map) => {
            msg[0] = match map_pages(
                window,
                thread,
                module,
                tables,
                context,
                NonZeroUsize::new(msg[2]),
                msg[3],
                msg[4],
                msg[5],
                msg[1],
            ) {
                Ok(()) => 0,
                Err(error) => error as usize,
            };
        }
        Ok(tau::Call::Unmap) => {
            msg[0] = match unmap_pages(
                window, thread, module, tables, context, msg[2], msg[3], msg[1],
            ) {
                Ok(()) => 0,
                Err(error) => error as usize,
            };
        }
        Ok(tau::Call::Debug) => {
            write!(
                sbi::Console,
                "SYS: checkpoint={} value=0x{:016x}\r\n",
                msg[1],
                msg[2]
            )
            .unwrap_or_default();
        }
        Err(a0) => {
            write!(sbi::Console, "cannot decode {a0:016x}\r\n").unwrap_or_default();
            loop {
                hint::spin_loop();
            }
        }
    }

    msg
}

fn valid_pages(address: usize, pages: usize) -> bool {
    address & 0xfff == 0
        && pages
            .checked_mul(4096)
            .is_some_and(|bytes| vmem::user_range(address, bytes))
}

fn overlaps_stem(module: &ModuleContext, address: usize, pages: usize) -> bool {
    if module.shared_index == usize::MAX {
        return false;
    }
    let Some(last) = pages
        .checked_mul(4096)
        .and_then(|bytes| bytes.checked_sub(1))
        .and_then(|n| address.checked_add(n))
    else {
        return false;
    };
    let first_index = (address >> 30) & 511;
    let last_index = (last >> 30) & 511;
    first_index <= module.shared_index && module.shared_index <= last_index
}

fn decode_flags(flags: usize) -> Option<[u8; 5]> {
    let r = flags & tau::PageFlags::READ.bits() != 0;
    let w = flags & tau::PageFlags::WRITE.bits() != 0;
    let x = flags & tau::PageFlags::EXEC.bits() != 0;
    if flags & !7 != 0 || !r || (w && x) {
        return None;
    }
    Some([
        b'r',
        if w { b'w' } else { b'-' },
        if x { b'x' } else { b'-' },
        b'u',
        b'-',
    ])
}

fn map_pages(
    window: &mut WindowAccess,
    thread: &Thread,
    module: &ModuleContext,
    tables: &ModuleTables,
    context: &Context,
    physical_addr: Option<NonZeroUsize>,
    virtual_addr: usize,
    number_of_pages: usize,
    raw_flags: usize,
    target: usize,
) -> Result<(), tau::AllocError> {
    let flags = decode_flags(raw_flags).ok_or(tau::AllocError::AccessDenied)?;
    if (!tables.loader_authority
        && (physical_addr.is_some() || target != 0 || raw_flags & tau::PageFlags::EXEC.bits() != 0))
        || (target == 0 && overlaps_stem(module, virtual_addr, number_of_pages))
    {
        return Err(tau::AllocError::AccessDenied);
    }
    if target == 0 {
        return alloc_pages(
            window,
            thread,
            context,
            physical_addr,
            virtual_addr,
            number_of_pages,
            flags,
        );
    }
    if !valid_pages(virtual_addr, number_of_pages)
        || physical_addr.is_some_and(|p| {
            p.get() & 0xfff != 0
                || number_of_pages
                    .checked_mul(4096)
                    .and_then(|n| p.get().checked_add(n))
                    .is_none()
        })
    {
        return Err(tau::AllocError::InvalidAddress);
    }
    let descriptor =
        loading::descriptor_page(context, target).map_err(|_| tau::AllocError::InvalidAddress)?;
    let dep = window.read_descriptor(descriptor);
    let last = virtual_addr + number_of_pages * 4096 - 1;
    if (virtual_addr >> 30) & 511 != dep.root_index || (last >> 30) & 511 != dep.root_index {
        return Err(tau::AllocError::InvalidAddress);
    }
    for i in 0..number_of_pages {
        if window
            .resolve_stem(dep.stem, virtual_addr + i * 4096)
            .is_some()
        {
            return Err(tau::AllocError::AlreadyAllocated);
        }
    }
    let mapping = Mapping {
        addr_virtual: virtual_addr,
        virtual_pages: number_of_pages,
        addr_physical: physical_addr,
        physical_pages: usize::from(physical_addr.is_some()) * number_of_pages,
        flags,
    };
    if let Err(error) = window.map_stem(dep.stem, mapping, || context.page(thread.hart_id())) {
        for i in 0..number_of_pages {
            let _ = window.unmap_stem(dep.stem, None, virtual_addr + i * 4096, |page| {
                context.free(thread.hart_id(), page)
            });
        }
        return Err(match error {
            llfree::Error::Address => tau::AllocError::InvalidAddress,
            llfree::Error::InvalidArgument => tau::AllocError::AlreadyAllocated,
            _ => tau::AllocError::OutOfMemory,
        });
    }
    if raw_flags & tau::PageFlags::EXEC.bits() != 0 {
        unsafe {
            core::arch::asm!("fence.i", options(nostack));
        }
    }
    Ok(())
}

fn unmap_pages(
    window: &mut WindowAccess,
    thread: &Thread,
    module: &ModuleContext,
    tables: &ModuleTables,
    context: &Context,
    virtual_addr: usize,
    number_of_pages: usize,
    target: usize,
) -> Result<(), tau::FreeError> {
    if target != 0 && !tables.loader_authority {
        return Err(tau::FreeError::AccessDenied);
    }
    if !valid_pages(virtual_addr, number_of_pages) {
        return Err(tau::FreeError::AlreadyFree);
    }
    if target == 0 {
        if overlaps_stem(module, virtual_addr, number_of_pages) {
            return Err(if tables.loader_authority {
                tau::FreeError::AlreadyFree
            } else {
                tau::FreeError::AccessDenied
            });
        }
        let root = Window::current_root();
        if !tables.loader_authority {
            for i in 0..number_of_pages {
                if window
                    .resolve(root, virtual_addr + i * 4096)
                    .is_some_and(|(_, pte)| pte & vmem::OWNED == 0)
                {
                    return Err(tau::FreeError::AccessDenied);
                }
            }
        }
        for i in 0..number_of_pages {
            let _ = window.unmap(root, virtual_addr + i * 4096, |page| {
                context.free(thread.hart_id(), page)
            });
        }
        return Ok(());
    }
    let descriptor =
        loading::descriptor_page(context, target).map_err(|_| tau::FreeError::InvalidAddress)?;
    let dep = window.read_descriptor(descriptor);
    let last = virtual_addr + number_of_pages * 4096 - 1;
    if (virtual_addr >> 30) & 511 != dep.root_index || (last >> 30) & 511 != dep.root_index {
        return Err(tau::FreeError::InvalidAddress);
    }
    for i in 0..number_of_pages {
        let _ = window.unmap_stem(dep.stem, None, virtual_addr + i * 4096, |page| {
            context.free(thread.hart_id(), page)
        });
    }
    Ok(())
}

pub fn alloc_pages(
    window: &mut WindowAccess,
    thread: &Thread,
    context: &Context,
    physical_addr: Option<NonZeroUsize>,
    virtual_addr: usize,
    number_of_pages: usize,
    flags: [u8; 5],
) -> Result<(), tau::AllocError> {
    if !valid_pages(virtual_addr, number_of_pages)
        || physical_addr.is_some_and(|p| {
            p.get() & 0xfff != 0
                || number_of_pages
                    .checked_mul(4096)
                    .and_then(|n| p.get().checked_add(n))
                    .is_none()
        })
    {
        return Err(tau::AllocError::InvalidAddress);
    }
    // Check the whole range first so a duplicate mapping cannot partially replace it.
    for i in 0..number_of_pages {
        if window
            .resolve(Window::current_root(), virtual_addr + i * 4096)
            .is_some()
        {
            return Err(tau::AllocError::AlreadyAllocated);
        }
    }
    let mapping = Mapping {
        addr_virtual: virtual_addr,
        virtual_pages: number_of_pages,
        addr_physical: physical_addr,
        physical_pages: usize::from(physical_addr.is_some()) * number_of_pages,
        flags,
    };

    let root = Window::current_root();
    if let Err(error) = window.map(root, mapping, || context.page(thread.hart_id())) {
        // Roll back pages allocated by this failed request; source physical
        // mappings are borrowed and are never returned to the allocator.
        for i in 0..number_of_pages {
            let _ = window.unmap(root, virtual_addr + i * 4096, |page| {
                context.free(thread.hart_id(), page)
            });
        }
        return Err(match error {
            llfree::Error::Address => tau::AllocError::InvalidAddress,
            llfree::Error::InvalidArgument => tau::AllocError::AlreadyAllocated,
            _ => tau::AllocError::OutOfMemory,
        });
    }
    Ok(())
}
