// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! ELF template preparation and lazy retained context construction. No language
//! interpretation, global invocation registry, or thread control block.

use core::{num::NonZeroUsize, ptr, slice, mem};

use elf::{
    ElfBytes,
    endian::LittleEndian,
    abi::{PT_LOAD, PF_R, PF_W, PF_X, ET_EXEC, EM_RISCV},
};
use tau::ModuleError;

use super::{
    module::{Dependency, ModuleContext, ModuleTables, CONTEXT_PAGES, TABLE_ORDER, TABLE_PAGES},
    scheduler::Thread,
    state::Context,
    llfree, mmu,
    vmem::{self, Mapping, Root, Window, WindowAccess, PAGE},
};

fn memory(_: llfree::Error) -> ModuleError {
    ModuleError::OutOfMemory
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

fn load_elf(
    window: &mut WindowAccess,
    context: &Context,
    hart: usize,
    root: Root,
    bytes: &[u8],
) -> Result<Dependency, ModuleError> {
    let elf =
        ElfBytes::<LittleEndian>::minimal_parse(bytes).map_err(|_| ModuleError::InvalidImage)?;
    if elf.ehdr.e_machine != EM_RISCV
        || elf.ehdr.e_type != ET_EXEC
        || elf.ehdr.class != elf::file::Class::ELF64
    {
        return Err(ModuleError::InvalidImage);
    }
    let manifest = image_range(&elf, elf.ehdr.e_entry as usize, size_of::<tau::Manifest>())?;
    let entry = word(manifest, mem::offset_of!(tau::Manifest, entry))?;
    let deps = mem::offset_of!(tau::Manifest, dependencies);
    if word(manifest, deps + 8)? != 0 {
        return Err(ModuleError::InvalidImage);
    }
    let regions = mem::offset_of!(tau::Manifest, mapped_regions);
    // First iteration supports one private stack, not declarative device bindings.
    if word(manifest, regions + 8)? != 1 {
        return Err(ModuleError::InvalidImage);
    }
    let stack = image_range(
        &elf,
        word(manifest, regions)?,
        size_of::<tau::MappedRegion>(),
    )?;
    let stack_pages = word(stack, mem::offset_of!(tau::MappedRegion, pages))?;
    if stack_pages == 0
        || stack_pages > 256
        || word(stack, mem::offset_of!(tau::MappedRegion, phys_start))? != 0
        || word(stack, mem::offset_of!(tau::MappedRegion, virtual_start))?
            != (stack_pages * PAGE).wrapping_neg()
        || stack.get(mem::offset_of!(tau::MappedRegion, write)) != Some(&1)
    {
        return Err(ModuleError::InvalidImage);
    }

    let mut root_index = None;
    let mut executable_entry = false;
    let mut total = 0usize;
    for ph in elf
        .segments()
        .ok_or(ModuleError::InvalidImage)?
        .iter()
        .filter(|p| p.p_type == PT_LOAD && p.p_memsz != 0)
    {
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
            || !vmem::user_range(address, ph.p_memsz as usize)
            || index == 511
            || index == vmem::SUPERVISOR_INDEX
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
        root_index = Some(index);
        executable_entry |=
            entry >= address && entry < end && ph.p_flags & PF_X != 0 && entry & 1 == 0;
        let file = elf
            .segment_data(&ph)
            .map_err(|_| ModuleError::InvalidImage)?;
        let first = address & !0xfff;
        let pages = (end - first).div_ceil(PAGE);
        let flags = [
            b'r',
            if ph.p_flags & PF_W != 0 { b'w' } else { b'-' },
            if ph.p_flags & PF_X != 0 { b'x' } else { b'-' },
            b'u',
            b'-',
        ];
        window
            .map(
                root,
                Mapping {
                    addr_virtual: first,
                    virtual_pages: pages,
                    flags,
                    ..Mapping::default()
                },
                || context.page(hart),
            )
            .map_err(|e| match e {
                llfree::Error::Address | llfree::Error::InvalidArgument => {
                    ModuleError::InvalidImage
                }
                _ => memory(e),
            })?;
        // Allocated pages are already zeroed. Copy only the segment's file bytes,
        // including correct handling of partial first/last pages and BSS tails.
        let mut done = 0;
        while done < file.len() {
            let virtual_address = address + done;
            let (physical, _) = window
                .resolve(root, virtual_address)
                .ok_or(ModuleError::InvalidImage)?;
            let offset = physical & 0xfff;
            let count = (PAGE - offset).min(file.len() - done);
            unsafe {
                ptr::copy_nonoverlapping(
                    file.as_ptr().add(done),
                    window.page_pointer(physical & !0xfff).add(offset),
                    count,
                )
            };
            done += count;
        }
    }
    if !executable_entry {
        return Err(ModuleError::InvalidImage);
    }
    let root_index = root_index.ok_or(ModuleError::InvalidImage)?;
    Ok(Dependency {
        stem: window
            .root_branch(root, root_index)
            .ok_or(ModuleError::InvalidImage)?,
        root_index,
        entry,
        stack_pages,
        tables: 0,
    })
}

pub fn prepare(
    window: &mut WindowAccess,
    context: &Context,
    hart: usize,
    tables: &ModuleTables,
    address: usize,
    length: usize,
) -> Result<u16, ModuleError> {
    if !tables.loader_authority {
        return Err(ModuleError::AccessDenied);
    }
    // Snapshot bounded user input before parsing. No unchecked userspace refs,
    // SUM, or dependency on the lifetime of the temporary source mapping.
    if length == 0 || length > 1024 * 1024 || !vmem::user_range(address, length) {
        return Err(ModuleError::InvalidImage);
    }
    let order = length.div_ceil(PAGE).next_power_of_two().ilog2() as usize;
    let root = window.create(0, || context.page(hart)).map_err(memory)?;
    let result = (|| {
        let snapshot = context.block(hart, order).map_err(memory)?;
        let data = window.block(snapshot, 1 << order);
        let bytes = unsafe { slice::from_raw_parts_mut(data, length) };
        let loaded = window
            .copy_user(Window::current_root(), address, bytes)
            .map_err(|_| ModuleError::InvalidImage)
            .and_then(|()| load_elf(window, context, hart, root, bytes));
        let _ = context.free_block(hart, snapshot, order);
        let mut dep = loaded?;
        let module_tables = context.block(hart, TABLE_ORDER).map_err(memory)?;
        let descriptor = match context.page(hart) {
            Ok(page) => page,
            Err(e) => {
                let _ = context.free_block(hart, module_tables, TABLE_ORDER);
                return Err(memory(e));
            }
        };
        let pointer = window
            .block(module_tables, 1 << TABLE_ORDER)
            .cast::<ModuleTables>();
        unsafe { ModuleTables::init_at(pointer, false) };
        dep.tables = module_tables;
        unsafe {
            window
                .page_pointer(descriptor)
                .cast::<Dependency>()
                .write(dep)
        };
        if let Some(slot) = tables.insert_dependency(descriptor) {
            // Publish newly copied executable bytes on this hart once.
            unsafe {
                core::arch::asm!("fence.i", options(nostack));
            }
            Ok(slot)
        } else {
            let _ = context.free(hart, descriptor);
            let _ = context.free_block(hart, module_tables, TABLE_ORDER);
            Err(ModuleError::NoSlots)
        }
    })();
    if result.is_err() {
        let _ = window.destroy(root, |page| context.free(hart, page));
    } else {
        // The published descriptor retains the stem; only the construction root
        // is temporary. Templates remain installed for the boot lifetime.
        let _ = context.free(hart, root.page());
    }
    result
}

pub fn invoke(
    window: &mut WindowAccess,
    context: &Context,
    thread: &Thread,
    module: &mut ModuleContext,
    slot: u16,
    descriptor: usize,
) -> Result<Root, ModuleError> {
    let caller = module.root;
    if module.module_id == descriptor {
        return Err(ModuleError::Reentrant);
    }
    // Follow protected caller links directly, without collecting invocations.
    let mut ancestor = module.caller();
    while let Some(root) = ancestor {
        let (physical, _) = window
            .resolve(root, vmem::MODULE_CONTEXT)
            .ok_or(ModuleError::InvalidImage)?;
        let (module_id, previous) = unsafe {
            ModuleContext::linkage_at(window.page_pointer(physical).cast::<ModuleContext>())
        };
        if module_id == descriptor {
            return Err(ModuleError::Reentrant);
        }
        ancestor = previous;
    }
    let root = match module.dependency(slot) {
        Some(root) => root,
        None => construct(window, context, thread, caller, descriptor)?,
    };
    module.retain_dependency(slot, root);
    Ok(root)
}

// Keep the hart-ID read and allocation code off the warm invocation path.
// Inlining lets LLVM hoist the thread load ahead of the cached-root check.
#[cold]
#[inline(never)]
fn construct(
    window: &mut WindowAccess,
    context: &Context,
    thread: &Thread,
    caller: Root,
    descriptor: usize,
) -> Result<Root, ModuleError> {
    let hart = thread.hart_id();
    let dep = unsafe { window.page_pointer(descriptor).cast::<Dependency>().read() };
    let root = window.create(0, || context.page(hart)).map_err(memory)?;
    let result = (|| {
        window.attach(root, dep.root_index, dep.stem);
        window
            .supervisor_mappings(caller, root, || context.page(hart))
            .map_err(memory)?;
        let (thread_page, _) = window
            .resolve(caller, vmem::THREAD)
            .ok_or(ModuleError::InvalidImage)?;
        for mapping in [
            Mapping {
                addr_virtual: vmem::THREAD,
                virtual_pages: 1,
                addr_physical: NonZeroUsize::new(thread_page),
                physical_pages: 1,
                flags: *b"rw---",
            },
            Mapping {
                addr_virtual: vmem::MODULE_CONTEXT,
                virtual_pages: CONTEXT_PAGES,
                flags: *b"rw---",
                ..Mapping::default()
            },
            Mapping {
                addr_virtual: vmem::MODULE,
                virtual_pages: TABLE_PAGES,
                addr_physical: NonZeroUsize::new(dep.tables),
                physical_pages: TABLE_PAGES,
                flags: *b"rw---",
            },
            Mapping {
                addr_virtual: (dep.stack_pages * PAGE).wrapping_neg(),
                virtual_pages: dep.stack_pages,
                flags: *b"rw-u-",
                ..Mapping::default()
            },
        ] {
            window
                .map(root, mapping, || context.page(hart))
                .map_err(memory)?;
        }
        let private = window
            .mapped_block(root, vmem::MODULE_CONTEXT, CONTEXT_PAGES)
            .ok_or(ModuleError::InvalidImage)?;
        let tag = mmu::allocate_tag(window).ok_or(ModuleError::OutOfMemory)?;
        let root = Root::sv39(root.page(), tag);
        unsafe {
            ModuleContext::init_at(private.cast(), root, dep.entry, descriptor, dep.root_index);
            // All registers start at zero, including sp (top of high stack).
        }
        Ok(root)
    })();
    if result.is_err() {
        let _ = window.destroy(root, |page| context.free(hart, page));
    }
    result
}
