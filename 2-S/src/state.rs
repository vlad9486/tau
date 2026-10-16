// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{arch, hint, num::NonZeroUsize, slice, ptr, mem};

use super::{
    vmem::{Window, WindowAccess, Mapping},
    module::{ModuleContext, ModuleTables},
    scheduler::Thread,
    llfree, loading, vmem, sbi, cpu, layout,
};

pub struct Context {
    pub allocator: llfree::Lower<'static>,
    pub base_addr: usize,
    /// Kept independently of the active satp for later loader/hart startup.
    pub bootstrap_root: vmem::Root,
}

const INTERRUPT_BIT: usize = 1 << (usize::BITS - 1);
const SUPERVISOR_TIMER: usize = 5;
const SUPERVISOR_EXTERNAL: usize = 9;
const PENDING_TIMER: usize = 1;
const PENDING_EXTERNAL: usize = 2;

fn take_pending_interrupt(thread: &mut Thread) -> Option<tau::Event> {
    if thread.pending_interrupts & PENDING_EXTERNAL != 0 {
        thread.pending_interrupts &= !PENDING_EXTERNAL;
        Some(tau::Event::Notified { arg: 0 })
    } else if thread.pending_interrupts & PENDING_TIMER != 0 {
        thread.pending_interrupts &= !PENDING_TIMER;
        Some(tau::Event::Timeout)
    } else {
        None
    }
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
        self.allocator.put(
            hart_id,
            llfree::FrameId((page - self.base_addr) >> 12),
            order,
        )
    }

    pub fn free(&self, hart_id: usize, page: usize) -> Result<(), llfree::Error> {
        self.free_block(hart_id, page, 0)
    }
}

#[derive(Debug)]
pub enum InitError {
    Elf(elf::ParseError),
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
    thread: &mut Thread,
    module: *mut ModuleContext,
    tables: *mut ModuleTables,
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
        .map(root, mapping, || context.page(thread.hart_id()))
        .map_err(|_| tau::AllocError::OutOfMemory)
        .map_err(InitError::Alloc)?;
    // The mapping is fresh; initialize the complete context value before access.
    unsafe { ModuleContext::init_at(module, root, 0, usize::MAX) };

    // Bootstrap constructs the initial module tables once. Future context
    // creation must reuse their backing pages, not allocate/reset these tables.
    let mapping = Mapping {
        addr_virtual: tables.addr(),
        virtual_pages: size_of::<ModuleTables>().div_ceil(0x1000),
        addr_physical: None,
        physical_pages: 0,
        flags: *b"rw---",
    };
    window
        .map(root, mapping, || context.page(thread.hart_id()))
        .map_err(|_| tau::AllocError::OutOfMemory)
        .map_err(InitError::Alloc)?;
    unsafe { ModuleTables::init_at(tables, true) };

    let elf_base = context.base_addr + (layout::SYSTEM_OFFSET << 12);
    let data = ptr::from_mut(window).cast::<u8>().with_addr(elf_base);
    let data = unsafe { slice::from_raw_parts(data, layout::SYSTEM_SIZE << 12) };

    use elf::{
        ElfBytes,
        endian::LittleEndian,
        abi::{PT_LOAD, PF_R, PF_W, PF_X},
    };
    let elf = ElfBytes::<LittleEndian>::minimal_parse(data).map_err(InitError::Elf)?;
    let mut manifest_segment = None;
    let mut manifest_range = 0..0;
    if let Some(s) = elf.segments() {
        for ph in s.iter().filter(|s| s.p_type == PT_LOAD) {
            if (ph.p_vaddr..(ph.p_vaddr + ph.p_memsz)).contains(&elf.ehdr.e_entry) {
                // manifest is in this section
                manifest_segment = Some(ph);
                let offset = (elf.ehdr.e_entry - ph.p_vaddr) as usize;
                manifest_range = offset..(offset + size_of::<tau::Manifest>());
            }
            let mut flags = *b"---u-";
            if ph.p_flags & PF_R != 0 {
                flags[0] = b'r';
            }
            if ph.p_flags & PF_W != 0 {
                flags[1] = b'w';
            }
            if ph.p_flags & PF_X != 0 {
                flags[2] = b'x';
            }
            let virtual_pages = ((ph.p_vaddr & 0xfff) + ph.p_memsz).div_ceil(0x1000) as usize;
            let physical_pages = ((ph.p_offset & 0xfff) + ph.p_filesz).div_ceil(0x1000) as usize;
            let mapping = Mapping {
                addr_virtual: (ph.p_vaddr as usize) & !0xfff,
                virtual_pages,
                addr_physical: NonZeroUsize::new((elf_base + ph.p_offset as usize) & !0xfff),
                physical_pages,
                flags,
            };

            window
                .map(root, mapping, || context.page(thread.hart_id()))
                .map_err(InitError::Allocator)?;
        }
    }

    if manifest_range.is_empty() {
        return Err(InitError::Manifest);
    }
    let Some(header) = manifest_segment else {
        return Err(InitError::Manifest);
    };
    let Ok(data) = elf.segment_data(&header) else {
        return Err(InitError::Manifest);
    };
    let Some(manifest) = data.get(manifest_range) else {
        return Err(InitError::Manifest);
    };
    let read_usize = |offset| unsafe {
        let entry = manifest
            .get_unchecked(offset..)
            .get_unchecked(..size_of::<usize>());
        usize::from_ne_bytes(entry.try_into().unwrap_unchecked())
    };

    let sl = mem::offset_of!(tau::Manifest, dependencies);
    let deps_vaddr = read_usize(sl);
    let deps_num = read_usize(sl + size_of::<usize>());

    let offset = deps_vaddr - header.p_vaddr as usize;
    for i in 0..deps_num {
        let range = (offset + i * size_of::<tau::ModuleId>())
            ..(offset + (i + 1) * size_of::<tau::ModuleId>());
        let Some(dep_repr) = data.get(range) else {
            return Err(InitError::Manifest);
        };
        let module_id = unsafe { *dep_repr.as_ptr().cast::<tau::ModuleId>() };
        // TODO: find the module and put in the table
        // tables.insert_dependency(...);
        let _ = module_id;
    }

    let sl = mem::offset_of!(tau::Manifest, mapped_regions);
    let ranges_vaddr = read_usize(sl);
    let ranges_num = read_usize(sl + size_of::<usize>());

    let offset = ranges_vaddr - header.p_vaddr as usize;
    for i in 0..ranges_num {
        let range = (offset + i * size_of::<tau::MappedRegion>())
            ..(offset + (i + 1) * size_of::<tau::MappedRegion>());
        let Some(range_repr) = data.get(range) else {
            return Err(InitError::Manifest);
        };
        let region = unsafe { *range_repr.as_ptr().cast::<tau::MappedRegion>() };
        alloc_pages(
            window,
            thread,
            context,
            region.phys_start,
            region.virtual_start as usize,
            region.pages,
        )
        .map_err(InitError::Alloc)?;
    }

    let sepc = read_usize(mem::offset_of!(tau::Manifest, entry));
    let satp = Window::current_root();

    Ok((satp, sepc))
}

/// A switch is performed after Rust references to context/module storage have
/// expired. The caller/callee metadata has different backing at the same VA.
pub struct Outcome {
    pub message: [usize; 6],
    pub root: Option<vmem::Root>,
    /// Initialize a fresh callee entry only after installing the destination root.
    pub enter_from: Option<vmem::Root>,
}

impl From<[usize; 6]> for Outcome {
    fn from(message: [usize; 6]) -> Self {
        Self {
            message,
            root: None,
            enter_from: None,
        }
    }
}

pub fn syscall(
    window: &mut WindowAccess,
    thread: &mut Thread,
    module: &mut ModuleContext,
    tables: &ModuleTables,
    context: &Context,
    mut msg: [usize; 6],
) -> Outcome {
    use core::fmt::Write;

    match tau::Call::decode(msg[0]) {
        Ok(tau::Call::PrepareModule) => {
            match loading::prepare(
                window,
                context,
                thread.hart_id(),
                tables,
                msg[3],
                msg[1],
                msg[2],
            ) {
                Ok((slot, handle)) => {
                    msg[0] = 0;
                    msg[1] = slot as usize;
                    msg[2] = handle;
                }
                Err(error) => {
                    msg[0] = error as usize;
                    msg[1] = 0;
                    msg[2] = 0;
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
        Ok(tau::Call::Invoke { slot, arg }) => {
            match loading::invoke(window, context, thread, module, tables, slot) {
                Ok(root) => {
                    msg[0] = arg as usize;
                    return Outcome {
                        message: msg,
                        root: Some(root),
                        enter_from: Some(module.root),
                    };
                }
                Err(error) => {
                    msg[0] = error as usize;
                }
            }
        }
        Ok(tau::Call::Respond { code }) => {
            msg[0] = code as usize;
            if let Some(caller) = module.take_caller() {
                return Outcome {
                    message: msg,
                    root: Some(caller),
                    enter_from: None,
                };
            } else {
                sbi::hart_stop().unwrap_or_default();
                loop {
                    hint::spin_loop();
                }
            }
        }
        Ok(tau::Call::Spawn { entry }) => {
            write!(sbi::Console, "spawn: {entry:016x}\r\n").unwrap_or_default();

            // TODO:
            let _ = entry;
            loop {
                hint::spin_loop();
            }
        }
        Ok(tau::Call::Exit) => {
            write!(sbi::Console, "exit\r\n").unwrap_or_default();

            // TODO:
            loop {
                hint::spin_loop();
            }
        }
        Ok(tau::Call::Join { thread_id }) => {
            write!(sbi::Console, "join {thread_id}\r\n").unwrap_or_default();

            // TODO:
            let _ = thread_id;
            loop {
                hint::spin_loop();
            }
        }
        Ok(tau::Call::Map) => {
            msg[0] = if (msg[1] != 0 && !tables.loader_authority)
                || overlaps_stem(module, msg[2], msg[3])
            {
                tau::AllocError::AccessDenied as usize
            } else {
                match alloc_pages(
                    window,
                    thread,
                    context,
                    NonZeroUsize::new(msg[1]),
                    msg[2],
                    msg[3],
                ) {
                    Ok(()) => 0,
                    Err(error) => error as usize,
                }
            };
        }
        Ok(tau::Call::Unmap) => {
            if !valid_pages(msg[1], msg[2]) || overlaps_stem(module, msg[1], msg[2]) {
                msg[0] = tau::FreeError::AlreadyFree as usize;
            } else {
                for i in 0..msg[2] {
                    let _ = window.unmap(Window::current_root(), msg[1] + i * 4096, |page| {
                        context.free(thread.hart_id(), page)
                    });
                }
                msg[0] = 0;
            }
        }
        Ok(tau::Call::Wait | tau::Call::Notify) => {
            // Phase-one ABI only. Do not pretend to park, notify, or bind an
            // interrupt until scheduler and waiter lifetime support exist.
            msg[0] = tau::WaitError::Unsupported as usize;
            msg[1] = 0;
        }
        Ok(tau::Call::WaitExternal) => {
            if let Some(event) = take_pending_interrupt(thread) {
                msg[0] = event.encode();
                return msg.into();
            }
            if msg[1] != 0 {
                sbi::set_timer(msg[1]).unwrap_or_default();
            }
            let cause = wait();
            let event = if cause == (INTERRUPT_BIT | SUPERVISOR_TIMER) {
                sbi::set_timer(usize::MAX).unwrap_or_default();
                // sbi::Printer.ch(*b"SU: timer\r\n");
                tau::Event::Timeout
            } else if cause == (INTERRUPT_BIT | SUPERVISOR_EXTERNAL) {
                cpu::csrrc!("sie", 1 << SUPERVISOR_EXTERNAL);
                // sbi::Printer.ch(*b"SU: interrupt\r\n");
                tau::Event::Notified { arg: 0 }
            } else if cause == (1 << 63) + 1 {
                // sbi::Printer.ch(*b"SU: software interrupt\r\n");
                // TODO:
                // An IPI is not an entry-point invocation. Scheduler/IPI
                // handling is still TODO; report generic readiness for now.
                tau::Event::Notified { arg: 0 }
            } else if cause & (1 << 63) != 0 {
                sbi::Printer.ch(*b"SU: unexpected interrupt\r\n");
                tau::Event::Notified { arg: 0 }
            } else {
                let lo = (cause % 10) as u8 + b'0';
                let hi = ((cause / 10) % 10) as u8 + b'0';
                sbi::Printer
                    .ch(*b"SU: exception: ")
                    .ch([lo, hi])
                    .ch(*b"\r\n");
                tau::asm::dbg([0xdeadbeef]);
                tau::Event::Notified { arg: 0 }
            };
            msg[0] = event.encode();
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

    msg.into()
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

pub fn alloc_pages(
    window: &mut WindowAccess,
    thread: &mut Thread,
    context: &Context,
    physical_addr: Option<NonZeroUsize>,
    virtual_addr: usize,
    number_of_pages: usize,
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
    // Check the whole range first so a duplicate mapping cannot partially replace it.
    for i in 0..number_of_pages {
        if window
            .resolve(Window::current_root(), virtual_addr + i * 4096)
            .is_some()
        {
            return Err(tau::AllocError::AlreadyAllocated);
        }
    }
    let mapping = Mapping {
        addr_virtual: virtual_addr,
        virtual_pages: number_of_pages,
        addr_physical: physical_addr,
        physical_pages: usize::from(physical_addr.is_some()) * number_of_pages,
        flags: *b"rw-u-",
    };

    let root = Window::current_root();
    if let Err(error) = window.map(root, mapping, || context.page(thread.hart_id())) {
        // Roll back pages allocated by this failed request; source physical
        // mappings are borrowed and are never returned to the allocator.
        for i in 0..number_of_pages {
            let _ = window.unmap(root, virtual_addr + i * 4096, |page| {
                context.free(thread.hart_id(), page)
            });
        }
        return Err(match error {
            llfree::Error::Address => tau::AllocError::InvalidAddress,
            llfree::Error::InvalidArgument => tau::AllocError::AlreadyAllocated,
            _ => tau::AllocError::OutOfMemory,
        });
    }
    Ok(())
}

fn wait() -> usize {
    let v;

    unsafe {
        arch::asm! {
            "li t1, 0x120",
            "csrrs t0, sstatus, t1",
            "li t1, 0x220",
            "csrrs t0, sie, t1",
            "la t0, 2f",
            "csrrw t2, sepc, t0",
            "la t0, 3f",
            "addi t0, t0, 3",
            "andi t0, t0, -4",
            "csrrw t3, stvec, t0",
            "sret",
            "2:",
            "wfi",
            "j 2b",
            "3:",
            "nop",
            "li t1, 0x120",
            "csrrc t0, sstatus, t1",
            "csrw sepc, t2",
            "csrw stvec, t3",
            "csrr {0}, scause",
            options(nomem, nostack),
            out(reg) v
        }
    }

    v
}

#[inline(always)]
pub fn handle_non_syscall_trap(cause: isize, thread: &mut Thread) {
    use core::fmt::Write;

    if cause >= 0 {
        let sepc = cpu::csrr!("sepc");
        let stval = cpu::csrr!("stval");

        write!(
            sbi::Console,
            "\r\n\nSU: exception: cause={cause} sepc=0x{sepc:016x} stval=0x{stval:016x}\r\n",
        )
        .unwrap_or_default();
        tau::asm::dbg([0xdeadbeef]);
    } else {
        match (cause as usize) & !INTERRUPT_BIT {
            SUPERVISOR_TIMER => {
                sbi::set_timer(usize::MAX).unwrap_or_default();
                thread.pending_interrupts |= PENDING_TIMER;
            }
            SUPERVISOR_EXTERNAL => {
                // Keep the level interrupt gated until userspace drains the PLIC.
                cpu::csrrc!("sie", 1 << SUPERVISOR_EXTERNAL);
                thread.pending_interrupts |= PENDING_EXTERNAL;
            }
            _ => {
                sbi::Printer.ch(*b"SU: unexpected interrupt\r\n");
            }
        };
    }
}
