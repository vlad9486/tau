// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{
    hint, mem,
    num::NonZero,
    ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};

use super::{
    space::{self, Window, WindowAccess, Mapping},
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

impl ModuleContext {
    /// # Safety
    /// `this` must point to writable, aligned storage with exclusive access
    /// until initialization completes.
    pub unsafe fn init_at(this: *mut Self, root: space::Root) {
        unsafe {
            ptr::addr_of_mut!((*this).header.registers).write([0; 32]);
            ptr::addr_of_mut!((*this).header.sepc).write(0);
            ptr::addr_of_mut!((*this).header.caller).write(None);
            ptr::addr_of_mut!((*this).header.root).write(root);
            ptr::addr_of_mut!((*this).header.entry).write(0);
        }
        let dependencies =
            unsafe { ptr::addr_of_mut!((*this).dependencies).cast::<Option<space::Root>>() };
        for slot in 0..0x800 {
            unsafe { dependencies.add(slot).write(None) };
        }
    }

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

#[repr(C)]
pub struct WaitTable {
    pub base: usize,
    pub count: usize,
    records: usize,
    order: u32,
}

impl WaitTable {
    pub const EMPTY: Self = Self {
        base: 0,
        count: 0,
        records: 0,
        order: 0,
    };

    pub fn unsafe_clone(&self) -> Self {
        WaitTable {
            base: self.base,
            count: self.count,
            records: self.records,
            order: self.order,
        }
    }
}

impl ModuleDescriptor {
    pub const EMPTY: Self = Self {
        stem: 0,
        root_index: None,
        entry: 0,
        stack_pages: 0,
        physical: 0,
    };
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

/// Supervisor-only metadata shared by all contexts of one module.
/// Dependency templates identify reusable shared branches of address spaces.
#[repr(C, align(4096))]
pub struct Module {
    pub header: ModuleHeader,
    // Slot zero is the free-list head. Free entries contain the next slot;
    // occupied entries contain a tagged physical descriptor address.
    dependencies: [AtomicUsize; 0x800],
}

impl Module {
    const END: usize = 0x800;
    const OCCUPIED: usize = 1 << (usize::BITS - 1);

    /// Initialize the module's shared tables once, before exposing its contexts.
    /// Write in place to avoid a large temporary on the supervisor stack.
    ///
    /// # Safety
    /// `this` must point to writable, aligned storage with exclusive access
    /// until initialization completes. Existing shared tables must not be reset
    /// when preparing another context of the same module.
    pub unsafe fn init_at(this: *mut Self, loader_authority: bool, pure: bool) {
        unsafe {
            ptr::addr_of_mut!((*this).header.descriptor).write(ModuleDescriptor::EMPTY);
            ptr::addr_of_mut!((*this).header.wait_table).write(WaitTable::EMPTY);
        }
        unsafe { ptr::addr_of_mut!((*this).header.loader_authority).write(loader_authority) };
        unsafe { ptr::addr_of_mut!((*this).header.pure).write(pure) };
        unsafe { ptr::addr_of_mut!((*this).header.poisoned).write(AtomicBool::new(false)) };
        unsafe { ptr::addr_of_mut!((*this).header.dependency_lock).write(AtomicBool::new(false)) };
        let dependencies = unsafe { ptr::addr_of_mut!((*this).dependencies).cast::<AtomicUsize>() };
        for index in 0..Self::END {
            unsafe { dependencies.add(index).write(AtomicUsize::new(index + 1)) };
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
}

struct DependencyGuard<'a>(&'a AtomicBool);

impl Drop for DependencyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

// ---

#[derive(Clone, Copy, Default)]
#[repr(C, align(32))]
pub struct WaitRecord {
    pub pending: bool,
    pub thread_id: usize,
    pub sequence: usize,
}

impl WaitTable {
    pub fn prepare(
        window: &mut WindowAccess,
        context: &Context,
        hart_id: usize,
        stem: usize,
        base: usize,
        count: usize,
    ) -> Result<Self, tau::ModuleError> {
        if count == 0 {
            return Ok(Self::EMPTY);
        }
        let bytes = count
            .checked_mul(64)
            .ok_or(tau::ModuleError::InvalidImage)?;
        if base == 0 || count == 0 || base & 63 != 0 || !space::user_range(base, bytes) {
            return Err(tau::ModuleError::InvalidImage);
        }
        let end = base
            .checked_add(bytes)
            .ok_or(tau::ModuleError::InvalidImage)?;
        if (base >> 30) & 511 != stem || ((end - 1) >> 30) & 511 != stem {
            return Err(tau::ModuleError::InvalidImage);
        }
        let storage = count
            .checked_mul(size_of::<WaitRecord>())
            .ok_or(tau::ModuleError::InvalidImage)?;
        let pages = storage
            .div_ceil(space::PAGE)
            .checked_next_power_of_two()
            .ok_or(tau::ModuleError::OutOfMemory)?;
        let order = pages.ilog2();
        let records = context
            .block(hart_id, order)
            .map_err(|_| tau::ModuleError::OutOfMemory)?;
        for page in 0..pages {
            window.page_mut(records + page * space::PAGE).fill(0);
        }
        Ok(Self {
            base,
            count,
            records,
            order,
        })
    }

    /// Used only when unpublished module construction rolls back.
    pub fn release(self, context: &Context, hart: usize) {
        if self.records != 0 {
            let _ = context.free_block(hart, self.records, self.order);
        }
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

    /// Read one record through a temporary physical alias.
    ///
    /// # Safety
    /// The table must be retained and `index < count`. The scheduler lock must
    /// protect record access. Records fit wholly within individual backing pages.
    pub unsafe fn read(&self, window: &mut WindowAccess, index: usize) -> WaitRecord {
        debug_assert!(index < self.count);
        let address = self.records + index * size_of::<WaitRecord>();
        let page = window.wait_mut(address & !(space::PAGE - 1));
        let offset = address & (space::PAGE - 1);
        unsafe { ptr::read(page[offset..].as_ptr().cast::<WaitRecord>()) }
    }

    /// Write one record through a temporary physical alias.
    ///
    /// # Safety
    /// The table must be retained and `index < count`. The scheduler lock must
    /// protect record access; no alias reference may survive this operation.
    pub unsafe fn write(&self, window: &mut WindowAccess, index: usize, record: WaitRecord) {
        debug_assert!(index < self.count);
        let address = self.records + index * size_of::<WaitRecord>();
        let page = window.wait_mut(address & !(space::PAGE - 1));
        let offset = address & (space::PAGE - 1);
        unsafe { ptr::write(page[offset..].as_mut_ptr().cast::<WaitRecord>(), record) };
    }
}

pub fn create_module(
    window: &mut WindowAccess,
    context: &Context,
    hart_id: usize,
    entry: usize,
    stack_pages: usize,
    wait_slots_addr: usize,
    wait_slots_count: usize,
) -> Result<usize, tau::ModuleError> {
    let root_index = (entry >> 30) & 511;
    if entry & 1 != 0
        || !space::user_range(entry, 2)
        || root_index == 511
        || root_index == space::SUPERVISOR_INDEX
        || !(1..=256).contains(&stack_pages)
    {
        return Err(tau::ModuleError::InvalidImage);
    }
    let wait_table = WaitTable::prepare(
        window,
        context,
        hart_id,
        root_index,
        wait_slots_addr,
        wait_slots_count,
    )?;
    let stem = match window.new_page(|| context.page(hart_id)) {
        Ok(page) => page,
        Err(_e) => {
            wait_table.release(context, hart_id);
            return Err(tau::ModuleError::OutOfMemory);
        }
    };
    let tables_pages = size_of::<Module>().div_ceil(space::PAGE);
    let physical = match context.block(hart_id, tables_pages.next_power_of_two().ilog2()) {
        Ok(block) => block,
        Err(_e) => {
            let _ = context.free(hart_id, stem);
            wait_table.release(context, hart_id);
            return Err(tau::ModuleError::OutOfMemory);
        }
    };
    let new_tables = window.map_module(physical);
    unsafe {
        Module::init_at(ptr::from_mut(new_tables), false, false);
        ptr::addr_of_mut!(new_tables.header.wait_table).write(wait_table);
        ptr::addr_of_mut!(new_tables.header.descriptor).write(ModuleDescriptor {
            stem,
            root_index: Some(root_index as u16),
            entry,
            stack_pages,
            physical,
        });
    }
    Ok(physical / space::PAGE)
}

pub fn bind(
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
        let physical = space::descriptor_page(context, parent)?;
        &*window.map_module(physical)
    };
    let descriptor = space::descriptor_page(context, dependency)?;
    module
        .insert_dependency(descriptor)
        .ok_or(tau::ModuleError::NoSlots)
}

#[inline(always)]
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
    let (root, entry) = construct(window, context, module_context.header.root, descriptor)?;
    module_context.retain_dependency(slot, root);
    Ok((root, entry))
}

// Keep the hart-ID read and allocation code off the warm invocation path.
// Inlining lets LLVM hoist the thread load ahead of the cached-root check.
#[cold]
#[inline(never)]
fn construct(
    window: &mut WindowAccess,
    context: &Context,
    caller: space::Root,
    descriptor: usize,
) -> Result<(space::Root, usize), tau::ModuleError> {
    let hart = window.hart().hart_id;
    let dep = window.read_module_descriptor(descriptor);
    let Some(root_index) = dep.root_index else {
        return Err(tau::ModuleError::InvalidImage);
    };
    let root_index = root_index as usize;
    if (dep.entry >> 30) & 511 != root_index
        || !window
            .resolve_stem(dep.stem, dep.entry)
            .is_some_and(|(_, flags)| flags & 26 == 26)
    {
        return Err(tau::ModuleError::InvalidImage);
    }
    let root = window
        .create(0, || context.page(hart))
        .map_err(|_| tau::ModuleError::OutOfMemory)?;
    let result = (|| {
        window.attach(root, root_index, dep.stem);
        window
            .supervisor_mappings(caller, root, || context.page(hart))
            .map_err(|_| tau::ModuleError::OutOfMemory)?;
        for mapping in [
            Mapping {
                addr_virtual: (dep.stack_pages * space::PAGE).wrapping_neg(),
                virtual_pages: dep.stack_pages,
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
                addr_physical: NonZero::new(dep.physical),
                physical_pages: size_of::<Module>().div_ceil(space::PAGE),
                flags: *b"rw---",
            },
        ] {
            window
                .map(root, mapping, || context.page(hart))
                .map_err(|_| tau::ModuleError::OutOfMemory)?;
        }
        let tag = window.allocate_tag().ok_or(tau::ModuleError::OutOfMemory)?;
        let root = space::Root::sv39(root.page(), tag);
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
    module: &Module,
    context: &Context,
    entry: usize,
) -> Result<space::Root, tau::SpawnError> {
    use tau::SpawnError;
    let caller = Window::current_root();
    let hart = window.hart().hart_id;
    let Some(index) = module.header.descriptor.root_index else {
        return Err(SpawnError::InvalidEntry);
    };
    let index = index as usize;
    if entry & 1 != 0 || (entry >> 30) & 511 != index {
        return Err(SpawnError::InvalidEntry);
    }
    let stem = module.header.descriptor.stem;
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
        let module_physical = module.header.descriptor.physical;
        for mapping in [
            Mapping {
                addr_virtual: (module.header.descriptor.stack_pages * space::PAGE).wrapping_neg(),
                virtual_pages: module.header.descriptor.stack_pages,
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
                addr_physical: NonZero::new(module_physical),
                physical_pages: size_of::<Module>().div_ceil(space::PAGE),
                flags: *b"rw---",
            },
        ] {
            window
                .map(root, mapping, || context.page(hart))
                .map_err(memory)?;
        }
        let tag = window.allocate_tag().ok_or(SpawnError::OutOfMemory)?;
        let root = space::Root::sv39(root.page(), tag);
        Ok(root)
    })();
    if result.is_err() {
        let _ = window.destroy(root, |page| context.free(hart, page));
    }
    result
}
