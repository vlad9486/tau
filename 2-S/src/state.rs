// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::num::NonZero;

use super::{
    llfree,
    matter::{ModuleDescriptor, ModuleContext, Module, ModuleSpec},
    space::{Root, Mapping, Window, WindowAccess, PAGE},
};

pub struct Context {
    pub allocator: llfree::Lower<'static>,
    pub base_addr: usize,
    /// Kept independently of the active satp for later loader/hart startup.
    pub bootstrap_root: Root,
    pub boot: ModuleSpec,
}

impl Context {
    pub fn block(&self, hart_id: usize, order: u32) -> Result<usize, llfree::Error> {
        let frame = self.allocator.alloc(hart_id, order as _)?.0;
        Ok(self.base_addr + (frame << 12))
    }

    pub fn page(&self, hart_id: usize) -> Result<usize, llfree::Error> {
        self.block(hart_id, 0)
    }

    pub fn free_block(&self, hart_id: usize, page: usize, order: u32) -> Result<(), llfree::Error> {
        if order > llfree::MAX_ORDER as u32 || page & ((1usize << (12 + order)) - 1) != 0 {
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
        self.allocator
            .put(hart_id, llfree::FrameId(frame), order as _)
    }

    pub fn free(&self, hart_id: usize, page: usize) -> Result<(), llfree::Error> {
        self.free_block(hart_id, page, 0)
    }
}

#[derive(Debug)]
pub enum InitError {
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
    hart_id: usize,
    module_context: *mut ModuleContext,
    module: *mut Module,
    context: &Context,
) -> Result<(Root, usize), InitError> {
    let (boot, stem) = context.boot.validate().map_err(|_| InitError::Manifest)?;
    let root = Window::current_root();

    let mapping = Mapping {
        addr_virtual: module_context.addr(),
        virtual_pages: size_of::<ModuleContext>().div_ceil(PAGE),
        addr_physical: None,
        physical_pages: 0,
        flags: *b"rw---",
    };

    window
        .map(root, mapping, || context.page(hart_id))
        .map_err(|_| tau::AllocError::OutOfMemory)
        .map_err(InitError::Alloc)?;

    // Bootstrap constructs the initial module tables once. Future context
    // creation must reuse their backing pages, not allocate/reset these tables.
    let pages = size_of::<Module>().div_ceil(PAGE);
    let module_physical = context
        .block(hart_id, pages.next_power_of_two().ilog2())
        .map_err(InitError::Allocator)?;
    let mapping = Mapping {
        addr_virtual: module.addr(),
        virtual_pages: pages,
        addr_physical: NonZero::new(module_physical),
        physical_pages: pages,
        flags: *b"rw---",
    };
    window
        .map(root, mapping, || context.page(hart_id))
        .map_err(|_| tau::AllocError::OutOfMemory)
        .map_err(InitError::Alloc)?;

    // The boot ELF uses the same protected shared-stem rule as prepared ELFs.
    let stem_phys = window.root_branch(root, stem).unwrap_or(0);
    let descriptor = ModuleDescriptor {
        stem: stem_phys,
        root_index: Some(stem as u16),
        entry: boot.entry,
        stack_pages: boot.stack_pages,
        physical: module_physical,
    };
    unsafe {
        Module::init_at(module, descriptor, boot.wait_table, true, false);
        ModuleContext::init_zeroed(module_context, root, boot.entry);
    }
    Ok((root, boot.entry))
}
