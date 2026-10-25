// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{num::NonZero, arch};

use super::{
    llfree, Context,
    vmem::{self, Mapping, Window, WindowAccess},
};

fn valid_pages(address: usize, pages: usize) -> bool {
    address & 0xfff == 0
        && pages
            .checked_mul(vmem::PAGE)
            .is_some_and(|bytes| vmem::user_range(address, bytes))
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
    if handle == 0 || page & (1 << (usize::BITS - 1)) != 0 || frame >= context.allocator.frames() {
        return Err(tau::ModuleError::InvalidDependency);
    }
    Ok(page)
}

/// Validate memory request from privileged module that maps/unmaps at `target` module.
fn validate_target(
    window: &mut WindowAccess,
    context: &Context,
    addr_virtual: usize,
    virtual_pages: usize,
    target: NonZero<usize>,
) -> Result<usize, ()> {
    let physical = validate_module_handle(context, target.get()).map_err(|_| ())?;
    let dep = window.read_module_descriptor(physical);
    let Some(root_index) = dep.root_index else {
        return Err(());
    };
    let root_index = root_index as usize;
    let last = addr_virtual + virtual_pages * vmem::PAGE - 1;
    if (addr_virtual >> 30) & 511 != root_index || (last >> 30) & 511 != root_index {
        return Err(());
    }

    Ok(dep.stem)
}

/// Validate memory mapping request from unprivileged module.
pub fn validate_map_unprivileged(mapping: &Mapping) -> Result<(), tau::AllocError> {
    if mapping.flags[2] != b'-' || mapping.addr_physical.is_some() {
        return Err(tau::AllocError::AccessDenied);
    }

    Ok(())
}

/// Validate memory mapping request.
pub fn validate_map(
    addr_physical: Option<NonZero<usize>>,
    addr_virtual: usize,
    virtual_pages: usize,
    raw_flags: usize,
) -> Result<Mapping, tau::AllocError> {
    let flags = decode_flags(raw_flags).ok_or(tau::AllocError::AccessDenied)?;
    if !valid_pages(addr_virtual, virtual_pages)
        || addr_physical.is_some_and(|p| {
            p.get() & 0xfff != 0
                || virtual_pages
                    .checked_mul(vmem::PAGE)
                    .and_then(|n| p.get().checked_add(n))
                    .is_none()
        })
    {
        return Err(tau::AllocError::InvalidAddress);
    }

    let mapping = Mapping {
        addr_virtual,
        virtual_pages,
        addr_physical,
        physical_pages: usize::from(addr_physical.is_some()) * virtual_pages,
        flags,
    };

    Ok(mapping)
}

pub fn map_pages(
    window: &mut WindowAccess,
    hart_id: usize,
    context: &Context,
    mapping: Mapping,
    target: Option<NonZero<usize>>,
) -> Result<(), tau::AllocError> {
    let addr_virtual = mapping.addr_virtual;
    let virtual_pages = mapping.virtual_pages;
    if let Some(target) = target {
        let stem = validate_target(window, context, addr_virtual, virtual_pages, target)
            .map_err(|()| tau::AllocError::InvalidAddress)?;
        if let Err(error) = window.map_stem(stem, mapping, || context.page(hart_id)) {
            if !matches!(
                error,
                llfree::Error::Address | llfree::Error::InvalidArgument
            ) {
                for i in 0..virtual_pages {
                    let virt = addr_virtual + i * vmem::PAGE;
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
pub fn validate_unmap(
    window: &mut WindowAccess,
    loader_authority: bool,
    virtual_addr: usize,
    number_of_pages: usize,
    target: usize,
) -> Result<(), tau::FreeError> {
    if !valid_pages(virtual_addr, number_of_pages) {
        return Err(tau::FreeError::AlreadyFree);
    }
    if target != 0 && !loader_authority {
        return Err(tau::FreeError::AccessDenied);
    }
    if target == 0 {
        let root = Window::current_root();
        if !loader_authority && !window.range_owned(root, virtual_addr, number_of_pages) {
            return Err(tau::FreeError::AccessDenied);
        }
    }

    Ok(())
}

pub fn unmap_pages(
    window: &mut WindowAccess,
    hart_id: usize,
    context: &Context,
    addr_virtual: usize,
    virtual_pages: usize,
    target: Option<NonZero<usize>>,
) -> Result<(), tau::FreeError> {
    if let Some(target) = target {
        let stem = validate_target(window, context, addr_virtual, virtual_pages, target)
            .map_err(|()| tau::FreeError::InvalidAddress)?;
        for i in 0..virtual_pages {
            let _ = window.unmap_stem(stem, None, addr_virtual + i * vmem::PAGE, |page| {
                context.free(hart_id, page)
            });
        }
    } else {
        let root = Window::current_root();
        for i in 0..virtual_pages {
            let _ = window.unmap(root, addr_virtual + i * vmem::PAGE, |page| {
                context.free(hart_id, page)
            });
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
            for i in 0..mapping.virtual_pages {
                let _ = window.unmap(root, mapping.addr_virtual + i * vmem::PAGE, |page| {
                    context.free(hart_id, page)
                });
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
