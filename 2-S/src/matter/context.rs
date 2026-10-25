// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{mem, num::NonZero, ptr};

use super::{
    space::{self, Window, WindowAccess, Mapping},
    module::{Module, ModuleDescriptor},
    Context,
};

/// Supervisor-only state private to one single-threaded module context.
/// The saved registers and PC resume this context after a dependency returns.
/// The caller's satp implicitly links invocations; no invocation list is needed.
/// Each dependency slot retains its own destination satp after first use.
#[repr(C, align(4096))]
pub struct ModuleContextHeader {
    /// saved execution context
    pub registers: [usize; 32],
    /// saved pc where userspace code is executing
    pub sepc: usize,
    /// address space root
    pub root: space::Root,
    /// address space root of the caller, is `None` for the `system` module
    caller: Option<space::Root>,
    /// cached entry point of the module
    pub entry: usize,
}

/// Module context is similar to a thread, but has its own address space
#[repr(C, align(4096))]
pub struct ModuleContext {
    pub header: ModuleContextHeader,
    /// cached address space roots of dependencies
    dependencies: [Option<space::Root>; 0x800],
}

// The complete context fits before __MODULE.
const _: () = assert!(size_of::<ModuleContext>() <= space::MODULE - space::MODULE_CONTEXT);

impl ModuleContext {
    /// Initialize a context whose backing pages have already been zeroed.
    ///
    /// # Safety
    /// The first page at `this` must be writable, aligned, and zeroed, and all
    /// remaining pages of `Self` must already be zeroed.
    pub unsafe fn init_zeroed(this: *mut Self, root: space::Root, entry: usize) {
        const {
            assert!(mem::offset_of!(Self, dependencies) <= space::PAGE);
            assert!(mem::size_of::<Option<space::Root>>() == mem::size_of::<usize>());
        }
        unsafe {
            ptr::addr_of_mut!((*this).header.sepc).write(entry);
            ptr::addr_of_mut!((*this).header.entry).write(entry);
            ptr::addr_of_mut!((*this).header.root).write(root);
        }
    }

    /// Set linkage and PC for a fresh invocation, retaining dependency roots.
    /// Saved registers remain stale until the next user trap; fresh entry must
    /// clear the hardware registers rather than restore this saved state.
    pub fn enter(&mut self, caller: space::Root) {
        self.header.sepc = self.header.entry;
        self.header.caller = Some(caller);
    }

    pub fn take_caller(&mut self) -> Option<space::Root> {
        self.header.caller.take()
    }

    pub fn dependency(&self, slot: u16) -> Option<space::Root> {
        self.dependencies.get(slot as usize).copied().flatten()
    }

    pub fn retain_dependency(&mut self, slot: u16, root: space::Root) {
        self.dependencies[slot as usize] = Some(root);
    }
}

pub fn invoke(
    window: &mut WindowAccess,
    context: &Context,
    module_context: &mut ModuleContext,
    module: &Module,
    slot: u16,
) -> Result<(space::Root, usize), tau::ModuleError> {
    let descriptor = module
        .get_dependency(slot)
        .ok_or(tau::ModuleError::InvalidDependency)?;
    let desc = window.read_module_descriptor(descriptor);
    let root = spawn(window, context, desc)?;
    module_context.retain_dependency(slot, root);
    Ok((root, desc.entry))
}

/// Construct a fresh module context without publishing it. Successful storage is
/// retained until reboot; all failed constructions reclaim their private pages.
pub fn spawn(
    window: &mut WindowAccess,
    context: &Context,
    desc: ModuleDescriptor,
) -> Result<space::Root, tau::SpawnError> {
    let caller = Window::current_root();
    let hart = window.hart().hart_id;
    let Some(index) = desc.root_index else {
        return Err(tau::SpawnError::InvalidEntry);
    };
    let index = index as usize;
    if desc.entry & 1 != 0
        || (desc.entry >> 30) & 511 != index
        || !window
            .resolve_stem(desc.stem, desc.entry)
            .is_some_and(|(_, flags)| flags & 26 == 26)
    {
        return Err(tau::SpawnError::InvalidEntry);
    }
    let root = window
        .create(0, || context.page(hart))
        .map_err(|_| tau::SpawnError::OutOfMemory)?;
    let result = (|| {
        window.attach(root, index, desc.stem);
        window
            .supervisor_mappings(caller, root, || context.page(hart))
            .map_err(|_| tau::SpawnError::OutOfMemory)?;
        for mapping in [
            Mapping {
                addr_virtual: (desc.stack_pages * space::PAGE).wrapping_neg(),
                virtual_pages: desc.stack_pages,
                flags: *b"rw-u-",
                ..Mapping::default()
            },
            Mapping {
                addr_virtual: space::MODULE_CONTEXT,
                virtual_pages: size_of::<ModuleContext>().div_ceil(space::PAGE),
                flags: *b"rw---",
                ..Mapping::default()
            },
            Mapping {
                addr_virtual: space::MODULE,
                virtual_pages: size_of::<Module>().div_ceil(space::PAGE),
                addr_physical: NonZero::new(desc.physical),
                physical_pages: size_of::<Module>().div_ceil(space::PAGE),
                flags: *b"rw---",
            },
        ] {
            window
                .map(root, mapping, || context.page(hart))
                .map_err(|_| tau::SpawnError::OutOfMemory)?;
        }
        let tag = window.allocate_tag().ok_or(tau::SpawnError::OutOfMemory)?;
        let root = space::Root::sv39(root.page(), tag);
        Ok(root)
    })();
    if result.is_err() {
        let _ = window.destroy(root, |page| context.free(hart, page));
    }
    result
}
