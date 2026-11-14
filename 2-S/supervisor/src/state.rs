// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::num::NonZero;

use super::{
    space, llfree,
    matter::{ModuleDescriptor, ModuleContext, Module, ModuleSpec, Capabilities},
    space::{Root, Mapping, MappingRange, Window, PAGE, ROOT_INDEX},
};

pub struct Context {
    /// Kept independently of the active satp for later loader/hart startup.
    pub bootstrap_root: Root,
    pub boot: ModuleSpec,
}

#[derive(Debug)]
pub enum InitError {
    Allocator(llfree::Error),
    Alloc(tau::AllocError),
    Manifest,
}

/// # Safety
/// The pointer must denote aligned linker-provided bootstrap address which
/// this function maps and initializes with exclusive access. `module` must
/// be initialized only once for this module.
pub unsafe fn init(module: *mut Module, context: &Context) -> Result<(), InitError> {
    let boot = context.boot.validate().map_err(|_| InitError::Manifest)?;
    let root = Window::current_root();
    let mut window = space::access();

    // Bootstrap constructs the initial module tables once. Future context
    // creation must reuse their backing pages, not allocate/reset these tables.
    let pages = size_of::<Module>().div_ceil(PAGE);
    let module_physical = window
        .hart()
        .block(pages.next_power_of_two().ilog2())
        .map_err(InitError::Allocator)?;
    let mapping = Mapping {
        range: MappingRange {
            addr: module.addr(),
            pages,
        },
        physical_addr: NonZero::new(module_physical),
        flags: *b"rw---",
    };
    window
        .map(root, mapping)
        .map_err(|_| tau::AllocError::OutOfMemory)
        .map_err(InitError::Alloc)?;

    // The boot ELF uses the same protected shared-stem rule as prepared ELFs.
    let stem_phys = window.root_branch(root, ROOT_INDEX).unwrap_or(0);
    let descriptor = ModuleDescriptor {
        stem_phys,
        entry: boot.entry,
        stack_pages: boot.stack_pages,
        physical: module_physical,
    };
    unsafe { Module::init_at(module, descriptor, boot.wait_table, Capabilities::LOADER) };

    Ok(())
}

/// # Safety
/// `module_context` must denote the aligned linker-provided context address
/// unmapped in the calling hart's fresh root, and `module` must already map
/// the shared bootstrap module tables.
pub unsafe fn init_hart(
    module_context: *mut ModuleContext,
    module: &Module,
) -> Result<usize, InitError> {
    let root = Window::current_root();
    let desc = module.header.descriptor;
    let mut window = space::access();
    for mapping in [
        Mapping {
            range: MappingRange {
                addr: (desc.stack_pages * PAGE).wrapping_neg(),
                pages: desc.stack_pages,
            },
            physical_addr: None,
            flags: *b"rw-u-",
        },
        Mapping {
            range: MappingRange {
                addr: module_context.addr(),
                pages: size_of::<ModuleContext>().div_ceil(PAGE),
            },
            physical_addr: None,
            flags: *b"rw---",
        },
    ] {
        window
            .map(root, mapping)
            .map_err(|_| tau::AllocError::OutOfMemory)
            .map_err(InitError::Alloc)?;
    }
    unsafe {
        ModuleContext::init_zeroed(module_context, root, desc.entry);
    }
    Ok(desc.entry)
}
