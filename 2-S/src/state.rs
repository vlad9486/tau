// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{hint, num::NonZeroUsize, ptr};

use super::{
    llfree, loading,
    module::{ModuleContext, ModuleTables},
    sbi,
    scheduler::{Scheduler, ThreadState},
    vmem::{self, Mapping, Window, WindowAccess},
    waiting::WaitTable,
};

#[derive(Clone, Copy, Default, Debug)]
#[repr(C)]
pub struct BootModule {
    pub entry: usize,
    pub stack_pages: usize,
    pub wait_slots_addr: usize,
    pub wait_slots_count: usize,
}

pub struct Context {
    pub allocator: llfree::Lower<'static>,
    pub base_addr: usize,
    /// Kept independently of the active satp for later loader/hart startup.
    pub bootstrap_root: vmem::Root,
    pub boot: BootModule,
}

impl Context {
    pub fn block(&self, hart_id: usize, order: usize) -> Result<usize, llfree::Error> {
        let frame = self.allocator.alloc(hart_id, order)?.0;
        Ok(self.base_addr + (frame << 12))
    }

    pub fn page(&self, hart_id: usize) -> Result<usize, llfree::Error> {
        self.block(hart_id, 0)
    }

    pub fn free_block(
        &self,
        hart_id: usize,
        page: usize,
        order: usize,
    ) -> Result<(), llfree::Error> {
        if order > llfree::MAX_ORDER || page & ((1usize << (12 + order)) - 1) != 0 {
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
        self.allocator.put(hart_id, llfree::FrameId(frame), order)
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
    thread: ThreadState,
    module: *mut ModuleContext,
    tables: *mut ModuleTables,
    scheduler: *mut Scheduler,
    context: &Context,
) -> Result<(vmem::Root, usize), InitError> {
    let root = Window::current_root();

    let mapping = Mapping {
        addr_virtual: module.addr(),
        virtual_pages: size_of::<ModuleContext>().div_ceil(0x1000),
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
    let tables_backing = context
        .block(hart_id, super::module::TABLE_ORDER)
        .map_err(InitError::Allocator)?;
    let mapping = Mapping {
        addr_virtual: tables.addr(),
        virtual_pages: size_of::<ModuleTables>().div_ceil(0x1000),
        addr_physical: NonZeroUsize::new(tables_backing),
        physical_pages: size_of::<ModuleTables>().div_ceil(0x1000),
        flags: *b"rw---",
    };
    window
        .map(root, mapping, || context.page(hart_id))
        .map_err(|_| tau::AllocError::OutOfMemory)
        .map_err(InitError::Alloc)?;
    unsafe { ModuleTables::init_at(tables, true, false) };

    let sepc = context.boot.entry;
    let stack_pages = context.boot.stack_pages;
    let satp = Window::current_root();
    let stem = (sepc >> 30) & 511;
    let wait_table = WaitTable::prepare(
        window,
        context,
        hart_id,
        stem,
        context.boot.wait_slots_addr,
        context.boot.wait_slots_count,
    )
    .map_err(|_| InitError::Manifest)?;
    // The boot ELF uses the same protected shared-stem rule as prepared ELFs.
    unsafe {
        ptr::addr_of_mut!((*tables).wait_table).write(wait_table);
        ptr::addr_of_mut!((*tables).stem_index).write(stem);
        ptr::addr_of_mut!((*tables).stack_pages).write(stack_pages);
        ModuleContext::init_zeroed(module, satp, sepc, 0, stem);
        Scheduler::initialize(scheduler, window, context, thread).map_err(InitError::Alloc)?;
    }
    Ok((satp, sepc))
}

#[inline]
pub fn syscall(
    window: &mut WindowAccess,
    module: &mut ModuleContext,
    tables: &ModuleTables,
    context: &Context,
    mut msg: [usize; 6],
) -> [usize; 6] {
    use core::fmt::Write;

    let hart_id = window.hart().hart_id;

    match tau::Call::decode(msg[0]) {
        Ok(tau::Call::AllocPhysical { order, zone: _ }) => {
            if !tables.loader_authority {
                msg[0] = tau::AllocError::AccessDenied as usize;
                msg[1] = 0;
            } else {
                match context.block(hart_id, order as usize) {
                    Ok(phys) => {
                        msg[0] = 0;
                        msg[1] = phys;
                    }
                    Err(_) => {
                        msg[0] = tau::AllocError::OutOfMemory as usize;
                        msg[1] = 0;
                    }
                }
            }
        }
        Ok(tau::Call::FreePhysical { order, zone: _ }) => {
            if !tables.loader_authority {
                msg[0] = tau::FreeError::AccessDenied as usize;
            } else {
                msg[0] = match context.free_block(hart_id, msg[1], order as usize) {
                    Ok(()) => 0,
                    Err(llfree::Error::Address) => tau::FreeError::InvalidAddress as usize,
                    Err(_) => tau::FreeError::AlreadyFree as usize,
                };
            }
        }
        Ok(tau::Call::CreateModule) => {
            match loading::create(
                window, context, hart_id, tables, msg[1], msg[2], msg[3], msg[4],
            ) {
                Ok(handle) => {
                    msg[0] = 0;
                    msg[1] = handle;
                }
                Err(error) => {
                    msg[0] = error as usize;
                    msg[1] = 0;
                }
            }
        }
        Ok(tau::Call::BindDependency) => {
            match loading::bind(window, context, tables, msg[1], msg[2]) {
                Ok(slot) => {
                    msg[0] = 0;
                    msg[1] = slot as usize;
                }
                Err(error) => {
                    msg[0] = error as usize;
                    msg[1] = 0;
                }
            }
        }
        // These calls need dispatch after all current-context borrows end.
        // The trap boundary handles them before entering here.
        Ok(
            tau::Call::Spawn { .. }
            | tau::Call::Exit
            | tau::Call::Wait
            | tau::Call::Notify
            | tau::Call::Invoke { .. }
            | tau::Call::Respond { .. },
        ) => {
            msg = [tau::WaitError::Unsupported as usize, 0, 0, 0, 0, 0];
        }
        Ok(tau::Call::Map) => {
            msg[0] = match map_pages(
                window,
                hart_id,
                module,
                tables,
                context,
                NonZeroUsize::new(msg[2]),
                msg[3],
                msg[4],
                msg[5],
                msg[1],
            ) {
                Ok(()) => 0,
                Err(error) => error as usize,
            };
        }
        Ok(tau::Call::Unmap) => {
            msg[0] = match unmap_pages(
                window, hart_id, module, tables, context, msg[2], msg[3], msg[1],
            ) {
                Ok(()) => 0,
                Err(error) => error as usize,
            };
        }
        Ok(tau::Call::Debug) => {
            write!(
                sbi::Console,
                "SYS: checkpoint={} value=0x{:016x}\r\n",
                msg[1],
                msg[2]
            )
            .unwrap_or_default();
        }
        Err(a0) => {
            write!(sbi::Console, "cannot decode {a0:016x}\r\n").unwrap_or_default();
            loop {
                hint::spin_loop();
            }
        }
    }

    msg
}

fn valid_pages(address: usize, pages: usize) -> bool {
    address & 0xfff == 0
        && pages
            .checked_mul(4096)
            .is_some_and(|bytes| vmem::user_range(address, bytes))
}

fn overlaps_stem(module: &ModuleContext, address: usize, pages: usize) -> bool {
    if module.shared_index == usize::MAX {
        return false;
    }
    let Some(last) = pages
        .checked_mul(4096)
        .and_then(|bytes| bytes.checked_sub(1))
        .and_then(|n| address.checked_add(n))
    else {
        return false;
    };
    let first_index = (address >> 30) & 511;
    let last_index = (last >> 30) & 511;
    first_index <= module.shared_index && module.shared_index <= last_index
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

fn map_pages(
    window: &mut WindowAccess,
    hart_id: usize,
    module: &ModuleContext,
    tables: &ModuleTables,
    context: &Context,
    physical_addr: Option<NonZeroUsize>,
    virtual_addr: usize,
    number_of_pages: usize,
    raw_flags: usize,
    target: usize,
) -> Result<(), tau::AllocError> {
    let flags = decode_flags(raw_flags).ok_or(tau::AllocError::AccessDenied)?;
    if (!tables.loader_authority
        && (physical_addr.is_some() || target != 0 || raw_flags & tau::PageFlags::EXEC.bits() != 0))
        || (target == 0 && overlaps_stem(module, virtual_addr, number_of_pages))
    {
        return Err(tau::AllocError::AccessDenied);
    }
    if target == 0 {
        return alloc_pages(
            window,
            hart_id,
            context,
            physical_addr,
            virtual_addr,
            number_of_pages,
            flags,
        );
    }
    if !valid_pages(virtual_addr, number_of_pages)
        || physical_addr.is_some_and(|p| {
            p.get() & 0xfff != 0
                || number_of_pages
                    .checked_mul(4096)
                    .and_then(|n| p.get().checked_add(n))
                    .is_none()
        })
    {
        return Err(tau::AllocError::InvalidAddress);
    }
    let descriptor =
        loading::descriptor_page(context, target).map_err(|_| tau::AllocError::InvalidAddress)?;
    let dep = window.read_descriptor(descriptor);
    let last = virtual_addr + number_of_pages * 4096 - 1;
    if (virtual_addr >> 30) & 511 != dep.root_index || (last >> 30) & 511 != dep.root_index {
        return Err(tau::AllocError::InvalidAddress);
    }
    let mapping = Mapping {
        addr_virtual: virtual_addr,
        virtual_pages: number_of_pages,
        addr_physical: physical_addr,
        physical_pages: usize::from(physical_addr.is_some()) * number_of_pages,
        flags,
    };
    if let Err(error) = window.map_stem(dep.stem, mapping, || context.page(hart_id)) {
        if !matches!(
            error,
            llfree::Error::Address | llfree::Error::InvalidArgument
        ) {
            for i in 0..number_of_pages {
                let _ = window.unmap_stem(dep.stem, None, virtual_addr + i * 4096, |page| {
                    context.free(hart_id, page)
                });
            }
        }
        return Err(match error {
            llfree::Error::Address => tau::AllocError::InvalidAddress,
            llfree::Error::InvalidArgument => tau::AllocError::AlreadyAllocated,
            _ => tau::AllocError::OutOfMemory,
        });
    }
    if raw_flags & tau::PageFlags::EXEC.bits() != 0 {
        unsafe {
            core::arch::asm!("fence.i", options(nostack));
        }
    }
    Ok(())
}

fn unmap_pages(
    window: &mut WindowAccess,
    hart_id: usize,
    module: &ModuleContext,
    tables: &ModuleTables,
    context: &Context,
    virtual_addr: usize,
    number_of_pages: usize,
    target: usize,
) -> Result<(), tau::FreeError> {
    if target != 0 && !tables.loader_authority {
        return Err(tau::FreeError::AccessDenied);
    }
    if !valid_pages(virtual_addr, number_of_pages) {
        return Err(tau::FreeError::AlreadyFree);
    }
    if target == 0 {
        if overlaps_stem(module, virtual_addr, number_of_pages) {
            return Err(if tables.loader_authority {
                tau::FreeError::AlreadyFree
            } else {
                tau::FreeError::AccessDenied
            });
        }
        let root = Window::current_root();
        if !tables.loader_authority && !window.range_owned(root, virtual_addr, number_of_pages) {
            return Err(tau::FreeError::AccessDenied);
        }
        for i in 0..number_of_pages {
            let _ = window.unmap(root, virtual_addr + i * 4096, |page| {
                context.free(hart_id, page)
            });
        }
        return Ok(());
    }
    let descriptor =
        loading::descriptor_page(context, target).map_err(|_| tau::FreeError::InvalidAddress)?;
    let dep = window.read_descriptor(descriptor);
    let last = virtual_addr + number_of_pages * 4096 - 1;
    if (virtual_addr >> 30) & 511 != dep.root_index || (last >> 30) & 511 != dep.root_index {
        return Err(tau::FreeError::InvalidAddress);
    }
    for i in 0..number_of_pages {
        let _ = window.unmap_stem(dep.stem, None, virtual_addr + i * 4096, |page| {
            context.free(hart_id, page)
        });
    }
    Ok(())
}

pub fn alloc_pages(
    window: &mut WindowAccess,
    hart_id: usize,
    context: &Context,
    physical_addr: Option<NonZeroUsize>,
    virtual_addr: usize,
    number_of_pages: usize,
    flags: [u8; 5],
) -> Result<(), tau::AllocError> {
    if !valid_pages(virtual_addr, number_of_pages)
        || physical_addr.is_some_and(|p| {
            p.get() & 0xfff != 0
                || number_of_pages
                    .checked_mul(4096)
                    .and_then(|n| p.get().checked_add(n))
                    .is_none()
        })
    {
        return Err(tau::AllocError::InvalidAddress);
    }
    let mapping = Mapping {
        addr_virtual: virtual_addr,
        virtual_pages: number_of_pages,
        addr_physical: physical_addr,
        physical_pages: usize::from(physical_addr.is_some()) * number_of_pages,
        flags,
    };

    let root = Window::current_root();
    if let Err(error) = window.map(root, mapping, || context.page(hart_id)) {
        // Roll back pages allocated by this failed request; source physical
        // mappings are borrowed and are never returned to the allocator.
        if !matches!(
            error,
            llfree::Error::Address | llfree::Error::InvalidArgument
        ) {
            for i in 0..number_of_pages {
                let _ = window.unmap(root, virtual_addr + i * 4096, |page| {
                    context.free(hart_id, page)
                });
            }
        }
        return Err(match error {
            llfree::Error::Address => tau::AllocError::InvalidAddress,
            llfree::Error::InvalidArgument => tau::AllocError::AlreadyAllocated,
            _ => tau::AllocError::OutOfMemory,
        });
    }
    Ok(())
}
