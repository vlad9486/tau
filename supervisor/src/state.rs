use core::{arch, hint, num::NonZeroUsize, slice};

use thiserror_no_std::Error;

use super::{
    vmem::{Window, Mapping},
    module::{ModuleTables, Invocation},
    scheduler::Thread,
    llfree, vmem,
};

pub struct Context {
    pub allocator: llfree::LLFree<'static>,
    pub base_addr: usize,
}

impl Context {
    pub fn page(&self, hart_id: usize) -> Result<usize, llfree::Error> {
        let frame = self.allocator.get(hart_id, llfree::Flags::o(0))?;
        Ok(self.base_addr + (frame << 12))
    }

    pub fn free(&self, hart_id: usize, page: usize) -> Result<(), llfree::Error> {
        let frame = (page - self.base_addr) >> 12;
        self.allocator.put(hart_id, frame, llfree::Flags::o(0))
    }
}

#[derive(Debug, Error)]
pub enum InitError {
    #[error("{0}")]
    Elf(#[from] elf::ParseError),
    #[error("{0:?}")]
    Allocator(#[from] llfree::Error),
    #[error("{0:?}")]
    Alloc(#[from] tau::AllocError),
    #[error("failed to parse manifest")]
    Manifest,
}

/// # Safety
/// `module` must be external static
pub unsafe fn init(
    hart_id: usize,
    window: &mut Window,
    thread: &mut Thread,
    module: *mut ModuleTables,
    context: &Context,
) -> Result<(vmem::Root, usize, tau::Inv), InitError> {
    let root = Window::current_root();
    let asid = root.asid();
    window.init(Window::current_root());

    let mapping = Mapping {
        addr_virtual: module.addr(),
        virtual_pages: size_of::<ModuleTables>() >> 12,
        addr_physical: None,
        physical_pages: 0,
        flags: *b"rw---",
    };

    window
        .map(asid, mapping, || context.page(thread.hart))
        .map_err(|_| tau::AllocError::OutOfMemory)?;
    unsafe { module.write_bytes(0, 1) };

    let module = unsafe { &*module };
    module.invocations.init();

    let elf_base = context.base_addr + (tau::loader::SYSTEM_OFFSET << 12);
    let data = (window as *mut Window).cast::<u8>().with_addr(elf_base);
    let data = unsafe { slice::from_raw_parts(data, tau::loader::SYSTEM_SIZE << 12) };

    use elf::{
        ElfBytes,
        endian::LittleEndian,
        abi::{PT_LOAD, PF_R, PF_W, PF_X},
    };
    let elf = ElfBytes::<LittleEndian>::minimal_parse(data)?;
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

            window.map(0, mapping, || context.page(hart_id))?;
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

    let sl = memoffset::offset_of!(tau::Manifest, dependencies);
    let deps_vaddr = read_usize(sl);
    let deps_num = read_usize(sl + size_of::<usize>());

    let offset = deps_vaddr - header.p_vaddr as usize;
    for i in 0..deps_num {
        let range = (offset + i * size_of::<tau::Dependency>())
            ..(offset + (i + 1) * size_of::<tau::Dependency>());
        let Some(dep_repr) = data.get(range) else {
            return Err(InitError::Manifest);
        };
        let tau::Dependency { name, slot, .. } = unsafe { *dep_repr.as_ptr().cast() };
        // TODO: find the module and put in the table
        // module.dependencies.put(slot, dependency);
        let _ = (name, slot);
    }

    let sl = memoffset::offset_of!(tau::Manifest, mapped_regions);
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
            region.virt_start as usize,
            region.pages,
        )?;
    }

    let satp = Window::current_root();
    let inv = unsafe {
        module
            .invocations
            .insert(Invocation {
                satp,
                sepc: context.base_addr,
                sp: 0,
            })
            .unwrap_unchecked()
    };

    let sepc = read_usize(memoffset::offset_of!(tau::Manifest, entry));
    let satp = Window::current_root();
    let inv = tau::Inv {
        inv,
        share: false,
        arg: 0,
    };

    Ok((satp, sepc, inv))
}

#[inline(always)]
pub fn syscall(
    window: &mut Window,
    thread: &mut Thread,
    module: &ModuleTables,
    context: &Context,
    mut msg: [usize; 6],
) -> [usize; 6] {
    match tau::Call::decode(msg[0]) {
        Ok(tau::Call::Invoke { slot, share, arg }) => {
            // TODO:
            let _ = (module.dependencies.get(slot), share, arg);
            loop {
                hint::spin_loop();
            }
        }
        Ok(tau::Call::Respond { inv, accept, code }) => {
            // TODO: find an invocation and return
            let _ = (module.invocations.get(inv), accept, code);
            loop {
                hint::spin_loop();
            }
        }
        Ok(tau::Call::Spawn { entry }) => {
            // TODO:
            let _ = entry;
            loop {
                hint::spin_loop();
            }
        }
        Ok(tau::Call::Exit) => {
            // TODO:
            loop {
                hint::spin_loop();
            }
        }
        Ok(tau::Call::Join { thread_id }) => {
            // TODO:
            let _ = thread_id;
            loop {
                hint::spin_loop();
            }
        }
        Ok(tau::Call::Map) => {
            msg[0] = match alloc_pages(
                window,
                thread,
                context,
                NonZeroUsize::new(msg[1]),
                msg[2],
                msg[3],
            ) {
                Ok(()) => 0,
                Err(err) => err as usize,
            };
        }
        Ok(tau::Call::Unmap) => {}
        Ok(tau::Call::Wait) => wait(),
        Err(_) => loop {
            hint::spin_loop();
        },
    }

    msg
}

pub fn alloc_pages(
    window: &mut Window,
    thread: &mut Thread,
    context: &Context,
    physical_addr: Option<NonZeroUsize>,
    virtual_addr: usize,
    number_of_pages: usize,
) -> Result<(), tau::AllocError> {
    let mapping = Mapping {
        addr_virtual: virtual_addr,
        virtual_pages: number_of_pages,
        addr_physical: physical_addr,
        physical_pages: usize::from(physical_addr.is_some()) * number_of_pages,
        flags: *b"rw-u-",
    };

    let asid = Window::current_root().asid();
    window
        .map(asid, mapping, || context.page(thread.hart))
        .map_err(|_| tau::AllocError::OutOfMemory)
}

fn wait() {
    use core::arch;

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
            "li t1, 0x220",
            "csrrc t0, sie, t1",
            "csrw sepc, t2",
            "csrw stvec, t3",
            options(nomem, nostack)
        }
    }
}

#[inline(always)]
pub fn exception(cause: isize) {
    let _ = cause;
    tau::dbg([0xdeadbeef]);
}
