// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{
    hint, ptr, mem,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::vmem;

/// Immutable prepared module descriptor, held in an owned physical page.
/// Shared module dependency slots contain its physical address.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Dependency {
    pub stem: usize,
    pub root_index: usize,
    pub entry: usize,
    pub stack_pages: usize,
    pub tables: usize,
}

pub const TABLE_PAGES: usize = size_of::<ModuleTables>().div_ceil(4096);
pub const TABLE_ORDER: usize = TABLE_PAGES.next_power_of_two().ilog2() as usize;
pub const CONTEXT_PAGES: usize = size_of::<ModuleContext>().div_ceil(vmem::PAGE);

/// Supervisor-only state private to one single-threaded module context.
/// The saved registers and PC resume this context after a dependency returns.
/// The caller's satp implicitly links invocations; no invocation list is needed.
/// Each dependency slot retains its own destination satp after first use.
#[repr(C)]
pub struct ModuleContext {
    pub registers: [usize; 32],
    pub sepc: usize,
    caller: Option<vmem::Root>,
    pub module_id: usize,
    pub shared_index: usize,
    pub root: vmem::Root,
    entry: usize,
    dependencies: [Option<vmem::Root>; 0x800],
}

// Header aliases expose one page; the complete context fits before __MODULE.
const _: () = assert!(mem::offset_of!(ModuleContext, dependencies) <= vmem::PAGE);
const _: () = assert!(CONTEXT_PAGES * vmem::PAGE <= vmem::MODULE - vmem::MODULE_CONTEXT);

impl ModuleContext {
    /// # Safety
    /// `this` must point to writable, aligned storage with exclusive access
    /// until initialization completes.
    pub unsafe fn init_at(
        this: *mut Self,
        root: vmem::Root,
        entry: usize,
        module_id: usize,
        shared_index: usize,
    ) {
        unsafe {
            ptr::addr_of_mut!((*this).registers).write([0; 32]);
            ptr::addr_of_mut!((*this).sepc).write(0);
            ptr::addr_of_mut!((*this).caller).write(None);
            ptr::addr_of_mut!((*this).module_id).write(module_id);
            ptr::addr_of_mut!((*this).shared_index).write(shared_index);
            ptr::addr_of_mut!((*this).root).write(root);
            ptr::addr_of_mut!((*this).entry).write(entry);
        }
        let dependencies =
            unsafe { ptr::addr_of_mut!((*this).dependencies).cast::<Option<vmem::Root>>() };
        for slot in 0..0x800 {
            unsafe { dependencies.add(slot).write(None) };
        }
    }

    /// Read only the first-page header through a temporary physical alias.
    /// # Safety
    /// The initialized header must be readable at `this`.
    pub unsafe fn linkage_at(this: *const Self) -> (usize, Option<vmem::Root>) {
        unsafe {
            (
                ptr::addr_of!((*this).module_id).read(),
                ptr::addr_of!((*this).caller).read(),
            )
        }
    }

    /// Set linkage and PC for a fresh invocation, retaining dependency roots.
    /// Saved registers remain stale until the next user trap; fresh entry must
    /// clear the hardware registers rather than restore this saved state.
    /// # Safety
    /// The initialized first-page header must be exclusively writable at `this`.
    pub unsafe fn enter_at(this: *mut Self, caller: vmem::Root) {
        unsafe {
            let entry = ptr::addr_of!((*this).entry).read();
            ptr::addr_of_mut!((*this).sepc).write(entry);
            ptr::addr_of_mut!((*this).caller).write(Some(caller));
        }
    }

    pub fn dependency(&self, slot: u16) -> Option<vmem::Root> {
        self.dependencies[slot as usize]
    }

    pub fn retain_dependency(&mut self, slot: u16, root: vmem::Root) {
        self.dependencies[slot as usize] = Some(root);
    }

    pub fn take_caller(&mut self) -> Option<vmem::Root> {
        self.caller.take()
    }

    pub fn caller(&self) -> Option<vmem::Root> {
        self.caller
    }
}

/// Supervisor-only metadata shared by all contexts of one module.
/// Dependency templates identify reusable shared branches, not thread contexts.
#[repr(C, align(4096))]
pub struct ModuleTables {
    pub loader_authority: bool,
    dependency_lock: AtomicBool,
    // Slot zero is the free-list head. Free entries contain the next slot;
    // occupied entries contain a tagged physical descriptor address.
    dependencies: [AtomicUsize; 0x800],
    // Phase two will define the protected waiter records and their generations.
    waiters_reserved: [AtomicUsize; 4096 / size_of::<AtomicUsize>()],
}

impl ModuleTables {
    const END: usize = 0x800;
    const OCCUPIED: usize = 1 << (usize::BITS - 1);

    /// Initialize the module's shared tables once, before exposing its contexts.
    /// Write in place to avoid a large temporary on the supervisor stack.
    ///
    /// # Safety
    /// `this` must point to writable, aligned storage with exclusive access
    /// until initialization completes. Existing shared tables must not be reset
    /// when preparing another context of the same module.
    pub unsafe fn init_at(this: *mut Self, loader_authority: bool) {
        unsafe { ptr::addr_of_mut!((*this).loader_authority).write(loader_authority) };
        unsafe { ptr::addr_of_mut!((*this).dependency_lock).write(AtomicBool::new(false)) };
        let dependencies = unsafe { ptr::addr_of_mut!((*this).dependencies).cast::<AtomicUsize>() };
        for index in 0..Self::END {
            unsafe { dependencies.add(index).write(AtomicUsize::new(index + 1)) };
        }
        let waiters = unsafe { ptr::addr_of_mut!((*this).waiters_reserved).cast::<AtomicUsize>() };
        for index in 0..4096 / size_of::<AtomicUsize>() {
            unsafe { waiters.add(index).write(AtomicUsize::new(0)) };
        }
    }

    fn lock_dependencies(&self) -> DependencyGuard<'_> {
        // Serialize the whole free-list operation, including publication of a
        // slot. Individual atomic cells alone do not make the free list safe.
        while self
            .dependency_lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            hint::spin_loop();
        }
        DependencyGuard(&self.dependency_lock)
    }

    pub fn insert_dependency(&self, descriptor: usize) -> Option<u16> {
        // Physical addresses must leave the tag bit unused.
        if descriptor == 0 || descriptor & (Self::OCCUPIED | 0xfff) != 0 {
            return None;
        }
        let _guard = self.lock_dependencies();
        let slot = self.dependencies[0].load(Ordering::Relaxed);
        if slot == Self::END {
            return None;
        }
        let cell = self.dependencies.get(slot)?;
        self.dependencies[0].store(cell.load(Ordering::Relaxed), Ordering::Relaxed);
        cell.store(descriptor | Self::OCCUPIED, Ordering::Relaxed);
        Some(slot as u16)
    }

    pub fn get_dependency(&self, slot: u16) -> Option<usize> {
        let _guard = self.lock_dependencies();
        self.get_dependency_locked(slot)
    }

    fn get_dependency_locked(&self, slot: u16) -> Option<usize> {
        let value = self
            .dependencies
            .get(slot as usize)?
            .load(Ordering::Relaxed);
        (slot != 0 && value & Self::OCCUPIED != 0).then_some(value & !Self::OCCUPIED)
    }
}

struct DependencyGuard<'a>(&'a AtomicBool);

impl Drop for DependencyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
