// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{
    hint, ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};

use super::{
    space::{self, WindowAccess},
    Context,
};

/// Immutable prepared module descriptor, held in an owned physical page.
/// Shared module dependency slots contain its physical address.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ModuleDescriptor {
    pub stem: usize,
    pub root_index: Option<u16>,
    pub entry: usize,
    pub stack_pages: usize,
    pub physical: usize,
}

#[repr(C, align(4096))]
pub struct ModuleHeader {
    pub descriptor: ModuleDescriptor,
    pub loader_authority: bool,
    pub pure: bool,
    pub poisoned: AtomicBool,
    dependency_lock: AtomicBool,
    pub wait_table: WaitTable,
}

#[derive(Clone, Copy, Default)]
#[repr(C, align(32))]
pub struct WaitRecord {
    pub pending: bool,
    pub thread_id: usize,
    pub sequence: usize,
}

/// Supervisor-only metadata shared by all contexts of one module.
/// Dependency templates identify reusable shared branches of address spaces.
#[repr(C, align(4096))]
pub struct Module {
    pub header: ModuleHeader,
    // Slot zero is the free-list head. Free entries contain the next slot;
    // occupied entries contain a tagged physical descriptor address.
    dependencies: [AtomicUsize; Self::END],
    wait_records: [WaitRecord; Self::WAIT_SIZE],
    // TODO:
    // wait_slots: [[u64; 8]; Self::WAIT_SIZE],
}

impl Module {
    const END: usize = 0x800;
    const OCCUPIED: usize = 1 << (usize::BITS - 1);

    pub const WAIT_SIZE: usize = 0x80;

    /// Initialize the module's shared tables once, before exposing its contexts.
    /// Write in place to avoid a large temporary on the supervisor stack.
    ///
    /// # Safety
    /// `this` must point to writable, aligned storage with exclusive access
    /// until initialization completes. Existing shared tables must not be reset
    /// when preparing another context of the same module.
    pub unsafe fn init_at(
        this: *mut Self,
        descriptor: ModuleDescriptor,
        wait_table: WaitTable,
        loader_authority: bool,
        pure: bool,
    ) {
        unsafe {
            ptr::addr_of_mut!((*this).header.descriptor).write(descriptor);
            ptr::addr_of_mut!((*this).header.wait_table).write(wait_table);
        }
        unsafe { ptr::addr_of_mut!((*this).header.loader_authority).write(loader_authority) };
        unsafe { ptr::addr_of_mut!((*this).header.pure).write(pure) };
        unsafe { ptr::addr_of_mut!((*this).header.poisoned).write(AtomicBool::new(false)) };
        unsafe { ptr::addr_of_mut!((*this).header.dependency_lock).write(AtomicBool::new(false)) };
        let dependencies = unsafe { ptr::addr_of_mut!((*this).dependencies).cast::<AtomicUsize>() };
        for index in 0..Self::END {
            unsafe { dependencies.add(index).write(AtomicUsize::new(index + 1)) };
        }
        let wait_table = unsafe { ptr::addr_of_mut!((*this).wait_records).cast::<WaitRecord>() };
        for index in 0..Self::WAIT_SIZE {
            unsafe { wait_table.add(index).write(WaitRecord::default()) };
        }
    }

    fn lock_dependencies(&self) -> DependencyGuard<'_> {
        // Serialize the whole free-list operation, including publication of a
        // slot. Individual atomic cells alone do not make the free list safe.
        while self
            .header
            .dependency_lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            hint::spin_loop();
        }
        DependencyGuard(&self.header.dependency_lock)
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
        // Publish the initialized descriptor to readers without taking the
        // free-list lock. Published slots remain occupied until shutdown.
        cell.store(descriptor | Self::OCCUPIED, Ordering::Release);
        Some(slot as u16)
    }

    pub fn get_dependency(&self, slot: u16) -> Option<usize> {
        let value = self
            .dependencies
            .get(slot as usize)?
            .load(Ordering::Acquire);
        (slot != 0 && value & Self::OCCUPIED != 0).then_some(value & !Self::OCCUPIED)
    }

    pub fn overlaps_stem(&self, address: usize, pages: usize) -> bool {
        let Some(index) = self.header.descriptor.root_index else {
            return false;
        };
        let Some(last) = pages
            .checked_mul(space::PAGE)
            .and_then(|bytes| bytes.checked_sub(1))
            .and_then(|n| address.checked_add(n))
        else {
            return false;
        };
        let first_index = ((address >> 30) & 511) as u16;
        let last_index = ((last >> 30) & 511) as u16;
        first_index <= index && index <= last_index
    }

    /// Read one record through a temporary physical alias.
    ///
    /// # Safety
    /// `index < count` must hold. The scheduler lock must
    /// protect record access.
    pub unsafe fn read_wait(&self, index: usize) -> WaitRecord {
        debug_assert!(index < self.header.wait_table.count);
        unsafe { *self.wait_records.get_unchecked(index) }
    }

    /// Write one record through a temporary physical alias.
    ///
    /// # Safety
    /// The table must be retained and `index < count`. The scheduler lock must
    /// protect record access; no alias reference may survive this operation.
    pub unsafe fn write_wait(&mut self, index: usize, record: WaitRecord) {
        debug_assert!(index < self.header.wait_table.count);
        unsafe { *self.wait_records.get_unchecked_mut(index) = record };
    }
}

struct DependencyGuard<'a>(&'a AtomicBool);

impl Drop for DependencyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

// ---

/// Range in userspace module's virtual memory
#[derive(Clone, Copy, Default, Debug)]
#[repr(C)]
pub struct WaitTable {
    pub base: usize,
    pub count: usize,
}

impl WaitTable {
    pub const EMPTY: Self = WaitTable { base: 0, count: 0 };

    pub fn validate(self, stem: usize) -> Result<Self, tau::ModuleError> {
        if self.count == 0 {
            return Ok(Self::EMPTY);
        }
        let bytes = self
            .count
            .checked_mul(64)
            .ok_or(tau::ModuleError::InvalidImage)?;
        if self.base == 0
            || self.count > Module::WAIT_SIZE
            || self.base & 63 != 0
            || !space::user_range(self.base, bytes)
        {
            return Err(tau::ModuleError::InvalidImage);
        }
        let end = self
            .base
            .checked_add(bytes)
            .ok_or(tau::ModuleError::InvalidImage)?;
        if (self.base >> 30) & 511 != stem || ((end - 1) >> 30) & 511 != stem {
            return Err(tau::ModuleError::InvalidImage);
        }
        Ok(self)
    }

    pub fn index(&self, pointer: usize) -> Result<usize, tau::WaitError> {
        let offset = pointer
            .checked_sub(self.base)
            .ok_or(tau::WaitError::InvalidWaiter)?;
        if self.count == 0 || offset & 63 != 0 || offset / 64 >= self.count {
            return Err(tau::WaitError::InvalidWaiter);
        }
        Ok(offset / 64)
    }

    pub fn occupied(
        &self,
        window: &mut WindowAccess,
        root: space::Root,
        index: usize,
    ) -> Result<bool, tau::WaitError> {
        let (physical, flags) = window
            .resolve(root, self.base + index * 64 + 63)
            .ok_or(tau::WaitError::InvalidWaiter)?;
        if flags & 30 != 22 {
            return Err(tau::WaitError::InvalidWaiter);
        }
        let byte = {
            let page = window.wait_mut(physical & !(space::PAGE - 1));
            let cell = unsafe {
                &*page[physical & (space::PAGE - 1)..]
                    .as_ptr()
                    .cast::<AtomicU8>()
            };
            cell.load(Ordering::SeqCst)
        };
        match byte {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(tau::WaitError::InvalidWaiter),
        }
    }
}

#[derive(Clone, Copy, Default, Debug)]
#[repr(C)]
pub struct ModuleSpec {
    pub entry: usize,
    pub stack_pages: usize,
    pub wait_table: WaitTable,
}

impl ModuleSpec {
    pub fn validate(mut self) -> Result<(Self, usize), tau::ModuleError> {
        let root_index = (self.entry >> 30) & 511;
        if self.entry & 1 != 0
            || !space::user_range(self.entry, 2)
            || root_index == 511
            || !(1..=256).contains(&self.stack_pages)
        {
            return Err(tau::ModuleError::InvalidImage);
        }
        self.wait_table = self.wait_table.validate(root_index)?;
        Ok((self, root_index))
    }
}

pub fn create_module(
    window: &mut WindowAccess,
    context: &Context,
    hart_id: usize,
    spec: ModuleSpec,
) -> Result<usize, tau::ModuleError> {
    let (spec, root_index) = spec.validate()?;
    let stem = window
        .new_page(|| context.page(hart_id))
        .map_err(|_| tau::ModuleError::OutOfMemory)?;
    let tables_pages = size_of::<Module>().div_ceil(space::PAGE);
    let physical = match context.block(hart_id, tables_pages.next_power_of_two().ilog2()) {
        Ok(block) => block,
        Err(_e) => {
            let _ = context.free(hart_id, stem);
            return Err(tau::ModuleError::OutOfMemory);
        }
    };
    let module = window.map_module(physical);
    let descriptor = ModuleDescriptor {
        stem,
        root_index: Some(root_index as u16),
        entry: spec.entry,
        stack_pages: spec.stack_pages,
        physical,
    };
    unsafe {
        Module::init_at(
            ptr::from_mut(module),
            descriptor,
            spec.wait_table,
            false,
            false,
        );
    }
    Ok(physical / space::PAGE)
}

pub fn bind_dependency(
    window: &mut WindowAccess,
    context: &Context,
    module: &Module,
    parent: usize,
    dependency: usize,
) -> Result<u16, tau::ModuleError> {
    if parent == dependency {
        return Err(tau::ModuleError::InvalidDependency);
    }
    // Longer cycles are the loader's responsibility. Slots and descriptors
    // remain installed until shutdown; existing context caches stay valid.
    let module = if parent == 0 {
        module
    } else {
        // Alias only at publication, after snapshot/construction aliases
        // have expired. No window remapping may occur while borrowed.
        let physical = space::validate_module_handle(context, parent)?;
        &*window.map_module(physical)
    };
    let physical = space::validate_module_handle(context, dependency)?;
    module
        .insert_dependency(physical)
        .ok_or(tau::ModuleError::NoSlots)
}
