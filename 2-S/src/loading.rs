// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Module template creation and lazy retained context construction. No language
//! interpretation, global invocation registry, or thread control block.

use core::{num::NonZeroUsize, ptr};

use tau::ModuleError;

use super::{
    module::{
        ModuleContext, ModuleTables, PreparedModule, CONTEXT_PAGES, TABLE_ORDER, TABLE_PAGES,
    },
    waiting::WaitTable,
    state::Context,
    llfree, mmu,
    vmem::{self, Mapping, Root, Window, WindowAccess, PAGE},
};

fn memory(_: llfree::Error) -> ModuleError {
    ModuleError::OutOfMemory
}

pub fn create(
    window: &mut WindowAccess,
    context: &Context,
    hart: usize,
    tables: &ModuleTables,
    entry: usize,
    stack_pages: usize,
    wait_slots_addr: usize,
    wait_slots_count: usize,
) -> Result<usize, ModuleError> {
    if !tables.loader_authority {
        return Err(ModuleError::AccessDenied);
    }
    let root_index = (entry >> 30) & 511;
    if entry & 1 != 0
        || !vmem::user_range(entry, 2)
        || root_index == 511
        || root_index == vmem::SUPERVISOR_INDEX
        || !(1..=256).contains(&stack_pages)
    {
        return Err(ModuleError::InvalidImage);
    }
    let wait_table = WaitTable::prepare(
        window,
        context,
        hart,
        root_index,
        wait_slots_addr,
        wait_slots_count,
    )?;
    let stem = match window.new_page(|| context.page(hart)) {
        Ok(page) => page,
        Err(e) => {
            wait_table.release(context, hart);
            return Err(memory(e));
        }
    };
    let module_tables = match context.block(hart, TABLE_ORDER) {
        Ok(block) => block,
        Err(e) => {
            let _ = context.free(hart, stem);
            wait_table.release(context, hart);
            return Err(memory(e));
        }
    };
    let new_tables = window.map_module_tables(module_tables);
    unsafe {
        ModuleTables::init_at(ptr::from_mut(new_tables), false, false);
        ptr::addr_of_mut!(new_tables.wait_table).write(wait_table);
        ptr::addr_of_mut!(new_tables.header).write(PreparedModule {
            stem,
            root_index: Some(root_index as u16),
            entry,
            stack_pages,
            tables: module_tables,
        });
    }
    Ok(module_tables / PAGE)
}

/// Handles are supplied by the trusted loader, not arbitrary module code.
/// Bounds checks prevent overflow and aliases outside managed physical memory;
/// they do not establish that the page contains a prepared descriptor.
pub fn descriptor_page(context: &Context, handle: usize) -> Result<usize, ModuleError> {
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
    context: &Context,
    parent: usize,
) -> Result<Option<usize>, ModuleError> {
    if parent == 0 {
        return Ok(None);
    }
    let descriptor = descriptor_page(context, parent)?;
    Ok(Some(descriptor))
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
    let parent_tables = parent_tables(context, parent)?;
    // Longer cycles are the loader's responsibility. Slots and descriptors
    // remain installed until shutdown; existing context caches stay valid.
    insert_dependency(window, tables, parent_tables, descriptor).ok_or(ModuleError::NoSlots)
}

#[inline(always)]
pub fn invoke(
    window: &mut WindowAccess,
    context: &Context,
    module: &mut ModuleContext,
    tables: &ModuleTables,
    slot: u16,
) -> Result<(Root, usize), ModuleError> {
    let descriptor = tables
        .get_dependency(slot)
        .ok_or(ModuleError::InvalidDependency)?;
    let (root, entry) = construct(window, context, module.root, descriptor)?;
    module.retain_dependency(slot, root);
    Ok((root, entry))
}

// Keep the hart-ID read and allocation code off the warm invocation path.
// Inlining lets LLVM hoist the thread load ahead of the cached-root check.
#[cold]
#[inline(never)]
fn construct(
    window: &mut WindowAccess,
    context: &Context,
    caller: Root,
    descriptor: usize,
) -> Result<(Root, usize), ModuleError> {
    let hart = window.hart().hart_id;
    let dep = window.read_descriptor(descriptor);
    let Some(root_index) = dep.root_index else {
        return Err(ModuleError::InvalidImage);
    };
    let root_index = root_index as usize;
    if (dep.entry >> 30) & 511 != root_index
        || !window
            .resolve_stem(dep.stem, dep.entry)
            .is_some_and(|(_, flags)| flags & 26 == 26)
    {
        return Err(ModuleError::InvalidImage);
    }
    let root = window.create(0, || context.page(hart)).map_err(memory)?;
    let result = (|| {
        window.attach(root, root_index, dep.stem);
        window
            .supervisor_mappings(caller, root, || context.page(hart))
            .map_err(memory)?;
        for mapping in [
            Mapping {
                addr_virtual: (dep.stack_pages * PAGE).wrapping_neg(),
                virtual_pages: dep.stack_pages,
                flags: *b"rw-u-",
                ..Mapping::default()
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
        ] {
            window
                .map(root, mapping, || context.page(hart))
                .map_err(memory)?;
        }
        let tag = mmu::allocate_tag(window).ok_or(ModuleError::OutOfMemory)?;
        let root = Root::sv39(root.page(), tag);
        Ok((root, dep.entry))
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
    module: &ModuleTables,
    context: &Context,
    entry: usize,
) -> Result<vmem::Root, tau::SpawnError> {
    use tau::SpawnError;
    let caller = Window::current_root();
    let hart = window.hart().hart_id;
    let Some(index) = module.header.root_index else {
        return Err(SpawnError::InvalidEntry);
    };
    let index = index as usize;
    if entry & 1 != 0 || (entry >> 30) & 511 != index {
        return Err(SpawnError::InvalidEntry);
    }
    let stem = module.header.stem;
    if !window
        .resolve_stem(stem, entry)
        .is_some_and(|(_, flags)| flags & 26 == 26)
    {
        return Err(SpawnError::InvalidEntry);
    }
    let memory = |_| SpawnError::OutOfMemory;
    let root = window.create(0, || context.page(hart)).map_err(memory)?;
    let result = (|| {
        window.attach(root, index, stem);
        window
            .supervisor_mappings(caller, root, || context.page(hart))
            .map_err(memory)?;
        let module_tables = module.header.tables;
        for mapping in [
            Mapping {
                addr_virtual: (module.header.stack_pages * PAGE).wrapping_neg(),
                virtual_pages: module.header.stack_pages,
                flags: *b"rw-u-",
                ..Mapping::default()
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
        ] {
            window
                .map(root, mapping, || context.page(hart))
                .map_err(memory)?;
        }
        let tag = mmu::allocate_tag(window).ok_or(SpawnError::OutOfMemory)?;
        let root = Root::sv39(root.page(), tag);
        Ok(root)
    })();
    if result.is_err() {
        let _ = window.destroy(root, |page| context.free(hart, page));
    }
    result
}
