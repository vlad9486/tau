// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{
    hint,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::vmem;

/// Dependency template shared by all contexts of the calling module.
/// `stem` is the physical address of the destination module's Sv39 branch below
/// the root, covering a 1 GiB virtual range of shared ELF/heap mappings. A call
/// uses this branch to construct a fresh root with private context mappings.
/// Entry-point, role, and declarative binding metadata remain TODO.
pub struct Dependency {
    pub stem: usize,
}

pub struct Invocation(pub vmem::Root);

/// Supervisor-only state private to one single-threaded module context.
/// Its caller is independent of the module-wide registry of suspended waiters.
#[repr(C)]
pub struct ModuleContext {
    caller: Option<Invocation>,
}

impl ModuleContext {
    /// # Safety
    /// `this` must point to writable, aligned storage with exclusive access
    /// until initialization completes.
    pub unsafe fn init_at(this: *mut Self, caller: Option<Invocation>) {
        unsafe { core::ptr::addr_of_mut!((*this).caller).write(caller) };
    }

    pub fn take_caller(&mut self) -> Option<Invocation> {
        self.caller.take()
    }
}

/// Supervisor-only metadata shared by all contexts of one module.
/// Dependency templates identify reusable shared branches, not thread contexts.
#[repr(C, align(4096))]
pub struct ModuleTables {
    dependency_lock: AtomicBool,
    // Slot zero is the free-list head. Free entries contain the next slot;
    // occupied entries contain a tagged physical stem address.
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
    pub unsafe fn init_at(this: *mut Self) {
        unsafe { core::ptr::addr_of_mut!((*this).dependency_lock).write(AtomicBool::new(false)) };
        let dependencies =
            unsafe { core::ptr::addr_of_mut!((*this).dependencies).cast::<AtomicUsize>() };
        for index in 0..Self::END {
            unsafe { dependencies.add(index).write(AtomicUsize::new(index + 1)) };
        }
        let waiters =
            unsafe { core::ptr::addr_of_mut!((*this).waiters_reserved).cast::<AtomicUsize>() };
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

    pub fn insert_dependency(&self, dep: Dependency) -> Option<u16> {
        // Physical addresses must leave the tag bit unused.
        if dep.stem & Self::OCCUPIED != 0 {
            return None;
        }
        let _guard = self.lock_dependencies();
        let slot = self.dependencies[0].load(Ordering::Relaxed);
        if slot == Self::END {
            return None;
        }
        self.dependencies[0].store(
            self.dependencies[slot].load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        self.dependencies[slot].store(dep.stem | Self::OCCUPIED, Ordering::Relaxed);
        Some(slot as u16)
    }

    pub fn remove_dependency(&self, slot: u16) {
        let _guard = self.lock_dependencies();
        if self.get_dependency_locked(slot).is_some() {
            self.dependencies[slot as usize].store(
                self.dependencies[0].load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            self.dependencies[0].store(slot as usize, Ordering::Relaxed);
        }
    }

    pub fn get_dependency(&self, slot: u16) -> Option<Dependency> {
        let _guard = self.lock_dependencies();
        self.get_dependency_locked(slot)
    }

    fn get_dependency_locked(&self, slot: u16) -> Option<Dependency> {
        let value = self
            .dependencies
            .get(slot as usize)?
            .load(Ordering::Relaxed);
        (slot != 0 && value & Self::OCCUPIED != 0).then_some(Dependency {
            stem: value & !Self::OCCUPIED,
        })
    }
}

struct DependencyGuard<'a>(&'a AtomicBool);

impl Drop for DependencyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
