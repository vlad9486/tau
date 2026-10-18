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
    module::{
        ModuleContext, ModuleTables, PreparedModule, CONTEXT_PAGES, TABLE_ORDER, TABLE_PAGES,
    },
    scheduler::{Thread, ReturnKind},
    waiting::WaitTable,
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
) -> Result<(PreparedModule, WaitTable), ModuleError> {
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
    // Validate the declared dependency slice's image backing. The trusted
    // loader resolves its names and publishes acyclic bindings before use.
    let dependency_count = word(manifest, deps + 8)?;
    if dependency_count != 0 {
        let bytes = dependency_count
            .checked_mul(size_of::<tau::ModuleId>())
            .ok_or(ModuleError::InvalidImage)?;
        image_range(&elf, word(manifest, deps)?, bytes)?;
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
                    window.page_mut(physical & !0xfff).as_mut_ptr().add(offset),
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
    let base = word(manifest, mem::offset_of!(tau::Manifest, wait_slots))?;
    let count = word(
        manifest,
        mem::offset_of!(tau::Manifest, wait_slots) + size_of::<usize>(),
    )?;
    let wait_table = WaitTable::prepare(window, context, hart, root, root_index, base, count)?;
    Ok((
        PreparedModule {
            stem: window
                .root_branch(root, root_index)
                .ok_or(ModuleError::InvalidImage)?,
            root_index,
            entry,
            stack_pages,
            tables: 0,
        },
        wait_table,
    ))
}

pub fn prepare(
    window: &mut WindowAccess,
    context: &Context,
    hart: usize,
    tables: &ModuleTables,
    parent: usize,
    address: usize,
    length: usize,
) -> Result<(u16, usize), ModuleError> {
    if !tables.loader_authority {
        return Err(ModuleError::AccessDenied);
    }
    let parent_tables = parent_tables(window, context, parent)?;
    // Snapshot bounded user input before parsing. No unchecked userspace refs,
    // SUM, or dependency on the lifetime of the temporary source mapping.
    if length == 0 || length > 1024 * 1024 || !vmem::user_range(address, length) {
        return Err(ModuleError::InvalidImage);
    }
    let order = length.div_ceil(PAGE).next_power_of_two().ilog2() as usize;
    let root = window.create(0, || context.page(hart)).map_err(memory)?;
    let mut wait_table = WaitTable::EMPTY;
    let result = (|| {
        let snapshot = context.block(hart, order).map_err(memory)?;
        let data = window.map_snapshot(snapshot, 1 << order);
        let bytes = unsafe { slice::from_raw_parts_mut(data, length) };
        let loaded = window
            .copy_user(Window::current_root(), address, bytes)
            .map_err(|_| ModuleError::InvalidImage)
            .and_then(|()| load_elf(window, context, hart, root, bytes));
        let _ = context.free_block(hart, snapshot, order);
        let (mut dep, wait) = loaded?;
        wait_table = wait;
        let module_tables = context.block(hart, TABLE_ORDER).map_err(memory)?;
        let descriptor = match context.page(hart) {
            Ok(page) => page,
            Err(e) => {
                let _ = context.free_block(hart, module_tables, TABLE_ORDER);
                return Err(memory(e));
            }
        };
        let new_tables = window.map_module_tables(module_tables);
        unsafe {
            ModuleTables::init_at(ptr::from_mut(new_tables), false, false);
            ptr::addr_of_mut!(new_tables.wait_table).write(wait_table);
            ptr::addr_of_mut!(new_tables.stem_index).write(dep.root_index);
            ptr::addr_of_mut!(new_tables.stack_pages).write(dep.stack_pages);
        };
        dep.tables = module_tables;
        window.write_descriptor(descriptor, dep);
        if let Some(slot) = insert_dependency(window, tables, parent_tables, descriptor) {
            // Publish newly copied executable bytes on this hart once.
            unsafe {
                core::arch::asm!("fence.i", options(nostack));
            }
            Ok((slot, descriptor / PAGE))
        } else {
            let _ = context.free(hart, descriptor);
            let _ = context.free_block(hart, module_tables, TABLE_ORDER);
            Err(ModuleError::NoSlots)
        }
    })();
    if result.is_err() {
        wait_table.release(context, hart);
        let _ = window.destroy(root, |page| context.free(hart, page));
    } else {
        // The published descriptor retains the stem; only the construction root
        // is temporary. Templates remain installed for the boot lifetime.
        let _ = context.free(hart, root.page());
    }
    result
}

/// Handles are supplied by the trusted loader, not arbitrary module code.
/// Bounds checks prevent overflow and aliases outside managed physical memory;
/// they do not establish that the page contains a prepared descriptor.
fn descriptor_page(context: &Context, handle: usize) -> Result<usize, ModuleError> {
    let page = handle
        .checked_mul(PAGE)
        .ok_or(ModuleError::InvalidDependency)?;
    let frame = page
        .checked_sub(context.base_addr)
        .ok_or(ModuleError::InvalidDependency)?
        / PAGE;
    if handle == 0 || page & (1 << (usize::BITS - 1)) != 0 || frame >= context.allocator.frames() {
        return Err(ModuleError::InvalidDependency);
    }
    Ok(page)
}

fn parent_tables(
    window: &mut WindowAccess,
    context: &Context,
    parent: usize,
) -> Result<Option<usize>, ModuleError> {
    if parent == 0 {
        return Ok(None);
    }
    let descriptor = descriptor_page(context, parent)?;
    let module = window.read_descriptor(descriptor);
    Ok(Some(module.tables))
}

fn insert_dependency(
    window: &mut WindowAccess,
    bootstrap: &ModuleTables,
    parent_tables: Option<usize>,
    descriptor: usize,
) -> Option<u16> {
    match parent_tables {
        None => bootstrap.insert_dependency(descriptor),
        Some(physical) => {
            // Alias only at publication, after snapshot/construction aliases
            // have expired. No window remapping may occur while borrowed.
            window
                .map_module_tables(physical)
                .insert_dependency(descriptor)
        }
    }
}

pub fn bind(
    window: &mut WindowAccess,
    context: &Context,
    tables: &ModuleTables,
    parent: usize,
    dependency: usize,
) -> Result<u16, ModuleError> {
    // Check the caller's authority before interpreting either physical handle.
    if !tables.loader_authority {
        return Err(ModuleError::AccessDenied);
    }
    if parent == dependency {
        return Err(ModuleError::InvalidDependency);
    }
    let descriptor = descriptor_page(context, dependency)?;
    let parent_tables = parent_tables(window, context, parent)?;
    // Longer cycles are the loader's responsibility. Slots and descriptors
    // remain installed until shutdown; existing context caches stay valid.
    insert_dependency(window, tables, parent_tables, descriptor).ok_or(ModuleError::NoSlots)
}

#[inline(always)]
pub fn invoke(
    window: &mut WindowAccess,
    context: &Context,
    thread: &Thread,
    module: &mut ModuleContext,
    tables: &ModuleTables,
    slot: u16,
) -> Result<Root, ModuleError> {
    let descriptor = tables
        .get_dependency(slot)
        .ok_or(ModuleError::InvalidDependency)?;
    let root = construct(window, context, thread, module.root, descriptor)?;
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
    let dep = window.read_descriptor(descriptor);
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
        let tag = mmu::allocate_tag(window).ok_or(ModuleError::OutOfMemory)?;
        let root = Root::sv39(root.page(), tag);
        let private = window
            .map_module_context(root)
            .ok_or(ModuleError::InvalidImage)?;
        unsafe {
            ModuleContext::init_at(ptr::from_mut(private), root, dep.entry, dep.root_index);
            // All registers start at zero, including sp (top of high stack).
        }
        Ok(root)
    })();
    if result.is_err() {
        let _ = window.destroy(root, |page| context.free(hart, page));
    }
    result
}

/// Construct a same-module thread without publishing it. Successful storage is
/// retained until reboot; all failed constructions reclaim their private pages.
pub fn spawn(
    window: &mut WindowAccess,
    thread: &Thread,
    module: &ModuleTables,
    context: &Context,
    entry: usize,
    _id: usize,
    mut message: [usize; 6],
) -> Result<usize, tau::SpawnError> {
    use tau::SpawnError;
    let caller = Window::current_root();
    let hart = thread.hart_id();
    let index = module.stem_index;
    if entry & 1 != 0
        || (entry >> 30) & 511 != index
        || !window
            .resolve(caller, entry)
            .is_some_and(|(_, flags)| flags & 26 == 26)
    {
        return Err(SpawnError::InvalidEntry);
    }
    let memory = |_| SpawnError::OutOfMemory;
    let root = window.create(0, || context.page(hart)).map_err(memory)?;
    let mut thread_page = 0;
    let result = (|| {
        let stem = window
            .root_branch(caller, index)
            .ok_or(SpawnError::InvalidEntry)?;
        window.attach(root, index, stem);
        window
            .supervisor_mappings(caller, root, || context.page(hart))
            .map_err(memory)?;
        thread_page = context.page(hart).map_err(memory)?;
        let module_tables = window
            .resolve(caller, vmem::MODULE)
            .ok_or(SpawnError::InvalidEntry)?
            .0;
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
                addr_physical: NonZeroUsize::new(module_tables),
                physical_pages: TABLE_PAGES,
                flags: *b"rw---",
            },
            Mapping {
                addr_virtual: (module.stack_pages * PAGE).wrapping_neg(),
                virtual_pages: module.stack_pages,
                flags: *b"rw-u-",
                ..Mapping::default()
            },
        ] {
            window
                .map(root, mapping, || context.page(hart))
                .map_err(memory)?;
        }
        let tag = mmu::allocate_tag(window).ok_or(SpawnError::OutOfMemory)?;
        let root = Root::sv39(root.page(), tag);
        let private = window
            .map_module_context(root)
            .ok_or(SpawnError::OutOfMemory)?;
        unsafe {
            ModuleContext::init_at(ptr::from_mut(private), root, entry, index);
            ptr::addr_of_mut!(private.sepc).write(entry);
        }
        message[0] = 0;
        let thread = Thread::new(hart, root, message, ReturnKind::Entry);
        window.write_thread(thread_page, thread);
        Ok(thread_page)
    })();
    if result.is_err() {
        let _ = window.destroy(root, |page| context.free(hart, page));
        if thread_page != 0 {
            let _ = context.free(hart, thread_page);
        }
    }
    result
}
