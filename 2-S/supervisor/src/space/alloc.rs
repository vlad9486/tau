// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{num::NonZero, arch};

use super::{
    llfree, Context, Module,
    vmem::{self, Mapping, Window, WindowAccess, MappingRange},
};

fn valid_pages(MappingRange { addr, pages }: MappingRange) -> bool {
    addr & 0xfff == 0
        && pages
            .checked_mul(vmem::PAGE)
            .is_some_and(|bytes| vmem::user_range(addr, bytes))
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

/// Handles are supplied by the trusted loader, not arbitrary module code.
/// Bounds checks prevent overflow and aliases outside managed physical memory;
/// they do not establish that the page contains a prepared descriptor.
pub fn validate_module_handle(context: &Context, handle: usize) -> Result<usize, tau::ModuleError> {
    let page = handle
        .checked_mul(vmem::PAGE)
        .ok_or(tau::ModuleError::InvalidDependency)?;
    let frame = page
        .checked_sub(context.base_addr)
        .ok_or(tau::ModuleError::InvalidDependency)?
        / vmem::PAGE;
    let module_pages = size_of::<Module>().div_ceil(vmem::PAGE);
    if handle == 0
        || page & (1 << (usize::BITS - 1)) != 0
        || frame
            .checked_add(module_pages)
            .is_none_or(|end| end > context.allocator.frames())
    {
        return Err(tau::ModuleError::InvalidDependency);
    }
    Ok(page)
}

/// Validate memory request from privileged module that maps/unmaps at `target` module
/// or at one of `target` module's bound dependency stems (`slot != 0`).
fn validate_target(
    window: &mut WindowAccess,
    context: &Context,
    module: &Module,
    MappingRange { addr, pages }: MappingRange,
    target: Option<NonZero<usize>>,
    slot: u16,
) -> Result<usize, ()> {
    let last = addr + (pages * vmem::PAGE - 1);
    if slot == 0 {
        let target = target.ok_or(())?;
        let physical = validate_module_handle(context, target.get()).map_err(|_| ())?;
        let dep = window.read_module_descriptor(physical);
        if vmem::addr_to_index(addr) != vmem::ROOT_INDEX
            || vmem::addr_to_index(last) != vmem::ROOT_INDEX
        {
            return Err(());
        }
        Ok(dep.stem_phys)
    } else {
        if vmem::addr_to_index(addr) != vmem::BINDING_INDEX
            || vmem::addr_to_index(last) != vmem::BINDING_INDEX
        {
            return Err(());
        }
        let target_module = match target {
            None => module,
            Some(target) => {
                let physical = validate_module_handle(context, target.get()).map_err(|_| ())?;
                &*window.map_module(physical)
            }
        };
        let (_, binding_stem) = target_module.get_dependency(slot).ok_or(())?;
        Ok(binding_stem)
    }
}

/// Validate memory mapping request.
pub fn validate_map(
    physical_addr: Option<NonZero<usize>>,
    addr: usize,
    pages: usize,
    raw_flags: usize,
) -> Result<Mapping, tau::AllocError> {
    let range = MappingRange { addr, pages };
    let flags = decode_flags(raw_flags).ok_or(tau::AllocError::AccessDenied)?;
    if !valid_pages(range)
        || physical_addr.is_some_and(|p| {
            p.get() & 0xfff != 0
                || range
                    .pages
                    .checked_mul(vmem::PAGE)
                    .and_then(|n| p.get().checked_add(n))
                    .is_none_or(|end| end > (1usize << 56))
        })
    {
        return Err(tau::AllocError::InvalidAddress);
    }

    let mapping = Mapping {
        range,
        physical_addr,
        flags,
    };

    Ok(mapping)
}

pub fn map_pages(
    window: &mut WindowAccess,
    hart_id: usize,
    context: &Context,
    module: &Module,
    mapping: Mapping,
    target: Option<NonZero<usize>>,
    slot: u16,
) -> Result<(), tau::AllocError> {
    if target.is_some() || slot != 0 {
        let stem = validate_target(window, context, module, mapping.range, target, slot)
            .map_err(|()| tau::AllocError::InvalidAddress)?;
        if let Err(error) = window.map_stem(stem, mapping, || context.page(hart_id)) {
            if !matches!(
                error,
                llfree::Error::Address | llfree::Error::InvalidArgument
            ) {
                for virt in mapping.range.pages() {
                    let _ = window.unmap_stem(stem, None, virt, |page| context.free(hart_id, page));
                }
            }
            return Err(map_err(error));
        }
    } else {
        alloc_pages(window, hart_id, context, mapping)?;
    }
    if mapping.flags[2] != b'-' {
        unsafe {
            arch::asm!("fence.i", options(nostack));
        }
    }
    Ok(())
}

/// Validate memory unmapping request.
pub fn validate_unmap(addr: usize, pages: usize) -> Result<MappingRange, tau::FreeError> {
    let range = MappingRange { addr, pages };
    if !valid_pages(range) {
        return Err(tau::FreeError::AlreadyFree);
    }

    Ok(range)
}

pub fn unmap_pages(
    window: &mut WindowAccess,
    hart_id: usize,
    context: &Context,
    module: &Module,
    range: MappingRange,
    target: Option<NonZero<usize>>,
    slot: u16,
) -> Result<(), tau::FreeError> {
    if target.is_some() || slot != 0 {
        let stem = validate_target(window, context, module, range, target, slot)
            .map_err(|()| tau::FreeError::InvalidAddress)?;
        for virt in range.pages() {
            let _ = window.unmap_stem(stem, None, virt, |page| context.free(hart_id, page));
        }
    } else {
        let root = Window::current_root();
        for virt in range.pages() {
            let _ = window.unmap(root, virt, |page| context.free(hart_id, page));
        }
        return Ok(());
    }
    Ok(())
}

fn alloc_pages(
    window: &mut WindowAccess,
    hart_id: usize,
    context: &Context,
    mapping: Mapping,
) -> Result<(), tau::AllocError> {
    let root = Window::current_root();
    if let Err(error) = window.map(root, mapping, || context.page(hart_id)) {
        // Roll back pages allocated by this failed request; source physical
        // mappings are borrowed and are never returned to the allocator.
        if !matches!(
            error,
            llfree::Error::Address | llfree::Error::InvalidArgument
        ) {
            for virt in mapping.range.pages() {
                let _ = window.unmap(root, virt, |page| context.free(hart_id, page));
            }
        }
        return Err(map_err(error));
    }
    Ok(())
}

fn map_err(error: llfree::Error) -> tau::AllocError {
    match error {
        llfree::Error::Address => tau::AllocError::InvalidAddress,
        llfree::Error::InvalidArgument => tau::AllocError::AlreadyAllocated,
        _ => tau::AllocError::OutOfMemory,
    }
}
