// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{
    cell::UnsafeCell,
    hint,
    num::NonZero,
    ptr,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::{
    space::{self, WindowAccess, MappingRange},
    Context,
};

/// Immutable prepared module descriptor, held in an owned physical page.
/// Shared module dependency slots contain its physical address.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ModuleDescriptor {
    pub stem_phys: usize,
    pub entry: usize,
    pub stack_pages: usize,
    pub physical: usize,
}

#[repr(C)]
pub struct DependencyEntry {
    descriptor: AtomicUsize,
    binding_stem_phys: AtomicUsize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Capabilities {
    // is the root system module, can create new modules
    pub loader: bool,
    // doesn't have global state that can be poisoned
    pub pure: bool,
    // can allocate physical memory and know the physical address, including DMA zone
    pub frame_alloc: bool,
    // can raise software irq
    pub raise: bool,
}

impl Capabilities {
    pub const LOADER: Self = Capabilities {
        loader: true,
        pure: false,
        frame_alloc: true,
        raise: true,
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IrqTarget {
    pub bit: NonZero<u8>,
    pub hart: u8,
}

#[repr(C, align(4096))]
pub struct ModuleHeader {
    pub descriptor: ModuleDescriptor,
    pub caps: Capabilities,
    pub poisoned: AtomicBool,
    dependency_lock: AtomicBool,
    pub wait_table: WaitTable,
    pub irqs: [Option<IrqTarget>; Module::MAX_IRQS],
}

#[derive(Clone, Copy, Default)]
#[repr(C, align(32))]
pub struct WaitRecord {
    pub pending: u64,
    pub thread_id: usize,
    pub sequence: usize,
}

/// Supervisor-only metadata shared by all contexts of one module.
/// Dependency templates identify reusable shared branches of address spaces.
#[repr(C, align(4096))]
pub struct Module {
    pub header: ModuleHeader,
    // Slot zero is the free-list head. Free entries contain the next slot;
    // occupied entries contain a tagged physical descriptor address and its binding stem.
    dependencies: [DependencyEntry; Self::MAX_DEPS],
    wait_records: [UnsafeCell<WaitRecord>; Self::WAIT_SIZE],
    // TODO:
    // wait_slots: [[u64; 8]; Self::WAIT_SIZE],
}

impl Module {
    pub const MAX_DEPS: usize = 0x800;
    pub const MAX_IRQS: usize = 16;
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
        caps: Capabilities,
    ) {
        unsafe {
            ptr::addr_of_mut!((*this).header.descriptor).write(descriptor);
            ptr::addr_of_mut!((*this).header.wait_table).write(wait_table);
        }
        unsafe { ptr::addr_of_mut!((*this).header.caps).write(caps) };
        unsafe { ptr::addr_of_mut!((*this).header.poisoned).write(AtomicBool::new(false)) };
        unsafe { ptr::addr_of_mut!((*this).header.dependency_lock).write(AtomicBool::new(false)) };
        unsafe { ptr::addr_of_mut!((*this).header.irqs).write([None; Self::MAX_IRQS]) };
        let dependencies =
            unsafe { ptr::addr_of_mut!((*this).dependencies).cast::<DependencyEntry>() };
        for index in 0..Self::MAX_DEPS {
            unsafe {
                dependencies.add(index).write(DependencyEntry {
                    descriptor: AtomicUsize::new(index + 1),
                    binding_stem_phys: AtomicUsize::new(0),
                });
            }
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

    pub fn insert_dependency(&self, descriptor: usize, binding_stem_phys: usize) -> Option<u16> {
        // Physical addresses must leave the tag bit unused.
        if descriptor == 0
            || descriptor & (Self::OCCUPIED | 0xfff) != 0
            || binding_stem_phys == 0
            || binding_stem_phys & 0xfff != 0
        {
            return None;
        }
        let _guard = self.lock_dependencies();
        let slot = self.dependencies[0].descriptor.load(Ordering::Relaxed);
        if slot == Self::MAX_DEPS {
            return None;
        }
        let cell = self.dependencies.get(slot)?;
        self.dependencies[0]
            .descriptor
            .store(cell.descriptor.load(Ordering::Relaxed), Ordering::Relaxed);
        cell.binding_stem_phys
            .store(binding_stem_phys, Ordering::Relaxed);
        // Publish the initialized descriptor to readers without taking the
        // free-list lock. Published slots remain occupied until shutdown.
        cell.descriptor
            .store(descriptor | Self::OCCUPIED, Ordering::Release);
        Some(slot as u16)
    }

    pub fn get_dependency(&self, slot: u16) -> Option<(usize, usize)> {
        let entry = self.dependencies.get(usize::from(slot))?;
        let value = entry.descriptor.load(Ordering::Acquire);
        if slot != 0 && value & Self::OCCUPIED != 0 {
            Some((
                value & !Self::OCCUPIED,
                entry.binding_stem_phys.load(Ordering::Relaxed),
            ))
        } else {
            None
        }
    }

    /// Chek if the `address` range overlaps protected regions.
    /// If it does, the memory operation is invalid at certain condition.
    pub fn overlaps_stem(&self, range: MappingRange) -> bool {
        // keep `_pages` parameter, we might need it in future
        let first_index = space::addr_to_index(range.addr);
        const _: () = {
            assert!(space::ROOT_INDEX == 0);
            assert!(space::BINDING_INDEX == 1);
        };
        first_index <= space::BINDING_INDEX
    }

    /// Read one record through a temporary physical alias.
    ///
    /// # Safety
    /// `index < count` must hold. The scheduler lock must
    /// protect record access.
    pub unsafe fn read_wait(&self, index: usize) -> WaitRecord {
        debug_assert!(index < self.header.wait_table.count);
        unsafe { *self.wait_records.get_unchecked(index).get() }
    }

    /// Write one record through a temporary physical alias.
    ///
    /// # Safety
    /// The table must be retained and `index < count`. The scheduler lock must
    /// protect record access; no alias reference may survive this operation.
    pub unsafe fn write_wait(&self, index: usize, record: WaitRecord) {
        debug_assert!(index < self.header.wait_table.count);
        unsafe { *self.wait_records.get_unchecked(index).get() = record };
    }

    pub fn get_irq_target(&self, index: usize) -> Option<IrqTarget> {
        self.header.irqs.get(index).copied().flatten()
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

    pub fn validate(self, stem: u16) -> Result<Self, tau::ModuleError> {
        if self.count == 0 {
            return Ok(Self::EMPTY);
        }
        let bytes = self
            .count
            .checked_mul(size_of::<tau::WaitSlot>())
            .ok_or(tau::ModuleError::InvalidImage)?;
        if self.base == 0
            || self.count > Module::WAIT_SIZE
            || self.base & (align_of::<tau::WaitSlot>() - 1) != 0
            || !space::user_range(self.base, bytes)
        {
            return Err(tau::ModuleError::InvalidImage);
        }
        let end = self
            .base
            .checked_add(bytes)
            .ok_or(tau::ModuleError::InvalidImage)?;

        if space::addr_to_index(self.base) != stem || space::addr_to_index(end - 1) != stem {
            return Err(tau::ModuleError::InvalidImage);
        }
        Ok(self)
    }

    pub fn index(&self, pointer: usize) -> Result<usize, tau::WaitError> {
        let offset = pointer
            .checked_sub(self.base)
            .ok_or(tau::WaitError::InvalidWaiter)?;
        let slot_size = size_of::<tau::WaitSlot>();
        if self.count == 0
            || offset & (align_of::<tau::WaitSlot>() - 1) != 0
            || offset / slot_size >= self.count
        {
            return Err(tau::WaitError::InvalidWaiter);
        }
        Ok(offset / slot_size)
    }

    pub fn occupied(
        &self,
        window: &mut WindowAccess,
        root: space::Root,
        index: usize,
    ) -> Result<bool, tau::WaitError> {
        let (physical, flags) = window
            .resolve(root, self.base + index * size_of::<tau::WaitSlot>())
            .ok_or(tau::WaitError::InvalidWaiter)?;
        if flags & 30 != 22 {
            return Err(tau::WaitError::InvalidWaiter);
        }
        let wait_page = window.wait(physical & !(space::PAGE - 1));
        let index = (physical & (space::PAGE - 1)) / size_of::<tau::WaitSlot>();
        wait_page[index].occupied()
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
    pub fn validate(mut self) -> Result<Self, tau::ModuleError> {
        let root_index = space::addr_to_index(self.entry);
        if self.entry & 1 != 0
            || !space::user_range(self.entry, 2)
            || root_index != space::ROOT_INDEX
            || !(1..=256).contains(&self.stack_pages)
        {
            return Err(tau::ModuleError::InvalidImage);
        }
        self.wait_table = self.wait_table.validate(space::ROOT_INDEX)?;
        Ok(self)
    }
}

pub fn create_module(
    window: &mut WindowAccess,
    context: &Context,
    hart_id: usize,
    spec: ModuleSpec,
    caps: tau::ModuleCapabilities,
) -> Result<usize, tau::ModuleError> {
    let spec = spec.validate()?;
    let stem_phys = window
        .new_page(|| context.page(hart_id))
        .map_err(|_| tau::ModuleError::OutOfMemory)?;
    let tables_pages = size_of::<Module>().div_ceil(space::PAGE);
    let physical = match context.block(hart_id, tables_pages.next_power_of_two().ilog2()) {
        Ok(block) => block,
        Err(_e) => {
            let _ = context.free(hart_id, stem_phys);
            return Err(tau::ModuleError::OutOfMemory);
        }
    };
    let module = window.map_module(physical);
    let descriptor = ModuleDescriptor {
        stem_phys,
        entry: spec.entry,
        stack_pages: spec.stack_pages,
        physical,
    };
    let caps = Capabilities {
        loader: false,
        pure: caps.contains(tau::ModuleCapabilities::PURE),
        frame_alloc: caps.contains(tau::ModuleCapabilities::FRAME_ALLOC),
        raise: caps.contains(tau::ModuleCapabilities::RAISE),
    };
    unsafe {
        Module::init_at(ptr::from_mut(module), descriptor, spec.wait_table, caps);
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
    let hart_id = window.hart().hart_id;
    let dependency_phys = space::validate_module_handle(context, dependency)?;
    let parent_phys = if parent == 0 {
        None
    } else {
        Some(space::validate_module_handle(context, parent)?)
    };
    let binding_stem_phys = window
        .new_page(|| context.page(hart_id))
        .map_err(|_| tau::ModuleError::OutOfMemory)?;
    // Longer cycles are the loader's responsibility. Slots and descriptors
    // remain installed until shutdown; existing context caches stay valid.
    let module = match parent_phys {
        None => module,
        Some(physical) => {
            // Alias only at publication, after snapshot/construction aliases
            // have expired. No window remapping may occur while borrowed.
            &*window.map_module(physical)
        }
    };
    if let Some(slot) = module.insert_dependency(dependency_phys, binding_stem_phys) {
        Ok(slot)
    } else {
        let _ = context.free(hart_id, binding_stem_phys);
        Err(tau::ModuleError::NoSlots)
    }
}

pub fn bind_irq(
    window: &mut WindowAccess,
    context: &Context,
    target: usize,
    region_index: usize,
    hart_id: usize,
    bit: usize,
) -> Result<(), tau::ModuleError> {
    let Some(bit) = u8::try_from(bit).ok().and_then(NonZero::new) else {
        return Err(tau::ModuleError::BadIrqBit);
    };
    if bit.get() >= 64 {
        return Err(tau::ModuleError::BadIrqBit);
    }
    if region_index >= Module::MAX_IRQS || hart_id >= 64 || target == 0 {
        return Err(tau::ModuleError::InvalidDependency);
    }
    let target_module = {
        let physical = space::validate_module_handle(context, target)?;
        window.map_module(physical)
    };
    let target = IrqTarget {
        bit,
        hart: hart_id as u8,
    };
    unsafe { *target_module.header.irqs.get_unchecked_mut(region_index) = Some(target) };
    Ok(())
}
