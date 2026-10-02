// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

#![no_std]
#![no_main]
#![cfg_attr(
    feature = "nightly",
    feature(custom_test_frameworks),
    test_runner(tau::tester::test_runner),
    feature(strict_provenance_lints),
    warn(fuzzy_provenance_casts)
)]

use core::{arch, cell::UnsafeCell, hint, mem::MaybeUninit, num::NonZero, slice};

use supervisor::{
    llfree::{Error, Allocator, FrameId},
    layout::{
        MODULES, DTB_OFFSET, DTB_PAGES, SUPERVISOR_OFFSET, SUPERVISOR_SIZE, SYSTEM_OFFSET,
        SYSTEM_SIZE, HEAP_START, HEAP_END,
    },
    sbi, vmem,
};

#[cfg(all(not(debug_assertions), feature = "panic-never"))]
use panic_never as _;

#[cfg(all(any(debug_assertions, not(feature = "panic-never")), not(test)))]
#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo) -> ! {
    loop {
        hint::spin_loop();
    }
}

unsafe extern "C" {
    static __ST: UnsafeCell<u8>;
    static __RO: UnsafeCell<u8>;
    static __HP: UnsafeCell<MaybeUninit<[usize; 0o1000]>>;
}

#[unsafe(link_section = ".text.init")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn _start() -> ! {
    arch::naked_asm! {
        "auipc t0, 0",
        "lui a2, %hi({st})",
        "add a2, a2, t0",
        "lui a3, %hi({ro})",
        "add a3, a3, t0",
        "lui a4, %hi({hp})",
        "add a4, a4, t0",
        "mv gp, a3",
        "mv sp, a4",
        "lui t0, 1",
        "add sp, sp, t0",
        "j {inner}",
        st = sym __ST,
        ro = sym __RO,
        hp = sym __HP,
        inner = sym inner,
    }
}

extern "C" fn inner(
    hart_id: usize,
    opaque: usize,
    st: *mut MaybeUninit<[usize; 512]>,
    ro: *mut MaybeUninit<[usize; 512]>,
    hp: *mut MaybeUninit<[usize; 512]>,
) -> ! {
    let (satp, info, info_pages, cores, frames) = if (opaque >> 56) == 0 {
        let dtb_addr = st.cast::<u32>().with_addr(opaque);
        let size = u32::from_be(unsafe { *dtb_addr.add(1) }) as usize;
        let dtb = unsafe { slice::from_raw_parts(dtb_addr, size / size_of::<u32>()) };
        let Ok((dtb, _)) = tau::Dtb::new(dtb) else {
            sbi::Printer.ch(*b"bad dtb\r\n");
            loop {
                hint::spin_loop();
            }
        };

        // Build a writable boot DTB in reserved RAM, without a heap allocator.
        let source = unsafe { slice::from_raw_parts(dtb_addr.cast::<u8>(), size) };
        let buffer = unsafe {
            slice::from_raw_parts_mut(st.cast::<u8>().add(DTB_OFFSET << 12), DTB_PAGES << 12)
        };
        let modules = MODULES.iter().filter(|m| m.user);
        let Ok(boot_size) = boot_dtb::with_modules(source, buffer, st.addr(), modules) else {
            sbi::Printer.ch(*b"bad boot dtb\r\n");
            loop {
                hint::spin_loop();
            }
        };
        let boot_addr = buffer.as_ptr().addr();
        match init_memory(hart_id, dtb_addr.addr(), dtb, st, ro, hp) {
            Ok((satp, cores, frames)) => {
                sbi::Printer.ch(*b"success\r\n");
                (satp, boot_addr, boot_size.div_ceil(0x1000), cores, frames)
            }
            Err(_err) => {
                sbi::Printer.ch(*b"failed\r\n");
                loop {
                    hint::spin_loop();
                }
            }
        }
    } else {
        // Additional harts enter through the loader with the bootstrap satp in
        // opaque. Reuse it so they see the same mappings as the boot hart when
        // enabling address translation and entering the supervisor, including
        // its already initialized system-wide data. This shared bootstrap root
        // is intentional; it does not define the eventual runtime storage scope.
        // Per-hart windows and supervisor stacks must be made private before
        // concurrent use. The runtime mapping transition is not yet implemented.
        (opaque, 0, 0, 0, 0)
    };

    unsafe {
        arch::asm! {
            "li t0, {ctx}",
            "csrw sscratch, t0",
            "li t0, {sepc}",
            "csrw sepc, t0",
            "li t0, 8",
            "csrw scause, t0",
            "li t0, 0x100",
            "csrrs t1, sstatus, t0", // set privileged mode
            "li t0, 0x20",
            "csrrc t1, sstatus, t0", // disable interrupts
            "csrw satp, {satp}",
            "sret",
            options(noreturn),
            in("a0") hart_id,
            in("a1") st.addr(),
            in("a2") info,
            in("a3") info_pages,
            in("a4") cores,
            in("a5") frames,
            satp = in(reg) satp,
            sepc = const { 0x_ffff_ffc0_0060_1000_usize },
            ctx = const { 0x_ffff_ffc0_0020_0000_usize },
        }
    }
}

fn init_memory(
    hart_id: usize,
    dtb_addr: usize,
    dtb: tau::Dtb<'_>,
    st: *mut MaybeUninit<[usize; 512]>,
    ro: *mut MaybeUninit<[usize; 512]>,
    hp: *mut MaybeUninit<[usize; 512]>,
) -> Result<(usize, usize, usize), Error> {
    let mut cores = 0;
    let mut memory_size = 0;
    for (props, path) in dtb.iter() {
        if path.len() == 3 && path[1] == "cpus" && path[2].starts_with("cpu@") {
            let status = props.find_str(|name| name == "status");
            cores += usize::from(status.is_some_and(|s| s.starts_with("okay")));
        } else if let Some(area) = props.find_reg()
            && path.len() == 2
            && path[1].starts_with("memory@")
        {
            memory_size = area.len;
        }
    }
    if cores < 2 || memory_size < 0x1000000 * cores {
        return Err(Error::Memory);
    }
    let Some(frames) = NonZero::new(memory_size >> 12) else {
        return Err(Error::Memory);
    };

    let ptr = unsafe { st.add(HEAP_END) };
    let Some(pages) = Allocator::expected_size(frames).map(|x| x.div_ceil(0x1000)) else {
        return Err(Error::Memory);
    };
    let mut allocator = unsafe { Allocator::new(frames, ptr.cast()) }.ok_or(Error::Memory)?;
    allocator.reserve_all();
    let allocator = allocator.as_lower();

    // beginning of free memory
    // `0x200` is opensbi size
    let mut frame = 0x200 + HEAP_END + pages;
    while frame & 0o777 != 0 {
        let core = ((frame >> 10) + hart_id) % cores;
        allocator.put(core, FrameId(frame), 0)?;
        frame += 1;
    }
    for big_frame in (frame / 0o1000)..(frames.get() / 0o1000) {
        let frame = big_frame << 9;
        // TODO: handle reserved regions properly
        if frame == dtb_addr >> 12 {
            continue;
        }
        let core = ((frame >> 10) + hart_id) % cores;
        allocator.put(core, FrameId(frame), 9)?;
    }

    let mut gfp = {
        let mut offset = HEAP_START;
        assert!(offset < HEAP_END);
        move || {
            offset += 1;
            unsafe { &mut *st.add(offset - 1) }.write([0; 0o1000])
        }
    };

    let l0 = gfp();
    let root_table = l0.as_mut_ptr().addr();

    // map self
    let l1 = gfp();
    let l2 = gfp();
    l0[(st.addr() >> 30) & 0o777] = (l1.as_mut_ptr().addr() >> 2) + 1;
    l1[(st.addr() >> 21) & 0o777] = (l2.as_mut_ptr().addr() >> 2) + 1;
    // assume doesn't cross 2MiB boundary
    for i in (st.addr() >> 12)..(ro.addr() >> 12) {
        l2[i & 0o777] = (i << 10) + vmem::flags_new(b"r-x--");
    }
    for i in (ro.addr() >> 12)..(hp.addr() >> 12) {
        l2[i & 0o777] = (i << 10) + vmem::flags_new(b"r----");
    }
    let system_start = (st.addr() >> 12) + SYSTEM_OFFSET;
    for i in system_start..(system_start + SYSTEM_SIZE) {
        l2[i & 0o777] = (i << 10) + vmem::flags_new(b"rw---");
    }

    // memory map for SV39:
    // 0x0000_0040_0000_0000 .. 0xffff_ffc0_0000_0000 (unavailable)
    // 0xffff_ffc0_0000_0000 .. 0xffff_ffc0_0020_0000 (2 MiB, per cpu, window, stack)
    // 0xffff_ffc0_0020_0000 .. 0xffff_ffc0_0040_0000 (2 MiB, registers and module metadata)
    // Registers remain in __THREAD for now, this is wrong; __MODULE_CONTEXT is private to the
    // active context. __MODULE holds shared dependency templates and reserved
    // waiter storage, mapped during supervisor bootstrap. Future contexts must
    // reuse the module's backing pages.
    // 0xffff_ffc0_0040_0000 .. 0xffff_ffc0_0060_0000 (2 MiB, global, scheduler)
    // 0xffff_ffc0_0060_0000 .. 0xffff_ffc0_006e_0000 (896 kiB, global, context and supervisor image)
    // 0xffff_ffc0_006e_0000 .. 0xffff_ffc0_0100_0000 (9 MiB 128 kiB, global, llfree allocator)
    // 0xffff_ffc0_0100_0000 .. 0xffff_ffc0_4000_0000 (1008 MiB, unused)
    // 0xffff_ffc0_4000_0000 .. 0x0000_0040_0000_0000 (511 GiB, user)

    let l1 = gfp();
    // the global branch, always `0o400`
    l0[0o400] = (l1.as_mut_ptr().addr() >> 2) + 1;

    let l2 = gfp();
    // the branch of the cpu context
    l1[0o000] = (l2.as_mut_ptr().addr() >> 2) + 1;
    // loop
    l2[0o000] = (l2.as_mut_ptr().addr() >> 2) + vmem::flags_new(b"rw---");

    // STATUS: size of the kernel stack is 1 page, increase if needed
    let stack = gfp();
    l2[0o777] = (stack.as_mut_ptr().addr() >> 2) + vmem::flags_new(b"rw---");

    let l2 = gfp();
    // the branch of the thread context
    l1[0o001] = (l2.as_mut_ptr().addr() >> 2) + 1;
    let thread = gfp();
    l2[0o000] = (thread.as_mut_ptr().addr() >> 2) + vmem::flags_new(b"rw---");

    // let l2 = gfp();
    // // the branch of the scheduler
    // l1[0o002] = (l2.as_mut_ptr().addr() >> 2) + vmem::flags_new(b"rw---");

    let mut l2 = gfp();
    // the branch of the supervisor image
    l1[0o003] = (l2.as_mut_ptr().addr() >> 2) + 1;
    l2[0o000] = (gfp().as_mut_ptr().addr() >> 2) + vmem::flags_new(b"rw--g");

    for p in 0..SUPERVISOR_SIZE {
        let virtual_page = 0x601 + p;
        l2[virtual_page % 0o1000] =
            ((st.addr() + ((SUPERVISOR_OFFSET + p) << 12)) >> 2) + vmem::flags_new(b"r-x-g");
    }
    for p in 0..pages {
        let virtual_page = 0x6e0 + p;
        let q = unsafe { l1.get_unchecked_mut(virtual_page / 0o1000) };
        if *q == 0 {
            l2 = gfp();
            *q = (l2.as_mut_ptr().addr() >> 2) + 1;
        }
        l2[virtual_page % 0o1000] = ((ptr.addr() + (p << 12)) >> 2) + vmem::flags_new(b"rw--g");
    }

    // format can be x (hex), d (signed decimal), u (unsigned decimal), o (octal), c (char) or i (asm instruction).
    // size can be b (8 bits), h (16 bits), w (32 bits) or g (64 bits).
    // xp/8gx 0x80200000

    Ok(((vmem::SV39 << 60) | (root_table >> 12), cores, frames.get()))
}

mod boot_dtb {
    //! Copy a firmware FDT and add /chosen/tau,modules in caller-reserved memory.
    //! No allocation, ELF parsing, or page-table operations are needed here.

    use supervisor::layout::StaticModule;

    #[derive(Debug, PartialEq, Eq)]
    pub enum Error {
        Invalid,
        NoSpace,
        ExistingModules,
    }

    const BEGIN_NODE: u32 = 1;
    const END_NODE: u32 = 2;
    const PROP: u32 = 3;
    const NOP: u32 = 4;
    const END: u32 = 9;
    const NAMES: &[u8] = b"#address-cells\0#size-cells\0tau,name\0reg\0";

    fn word(bytes: &[u8], at: usize) -> Result<u32, Error> {
        let bytes = bytes
            .get(at..at.checked_add(4).ok_or(Error::Invalid)?)
            .ok_or(Error::Invalid)?;
        Ok(u32::from_be_bytes(
            bytes.try_into().map_err(|_| Error::Invalid)?,
        ))
    }

    fn span(bytes: &[u8], start: usize, len: usize) -> Result<&[u8], Error> {
        bytes
            .get(start..start.checked_add(len).ok_or(Error::Invalid)?)
            .ok_or(Error::Invalid)
    }

    fn padded(len: usize) -> Result<usize, Error> {
        Ok(len.checked_add(3).ok_or(Error::Invalid)? & !3)
    }

    struct Writer<'a> {
        bytes: &'a mut [u8],
        pos: usize,
    }

    impl Writer<'_> {
        fn bytes(&mut self, value: &[u8]) -> Result<(), Error> {
            let end = self.pos.checked_add(value.len()).ok_or(Error::NoSpace)?;
            self.bytes
                .get_mut(self.pos..end)
                .ok_or(Error::NoSpace)?
                .copy_from_slice(value);
            self.pos = end;
            Ok(())
        }

        fn word(&mut self, value: u32) -> Result<(), Error> {
            self.bytes(&value.to_be_bytes())
        }

        fn align(&mut self) -> Result<(), Error> {
            while self.pos & 3 != 0 {
                self.bytes(&[0])?;
            }
            Ok(())
        }

        fn node(&mut self, name: &[u8]) -> Result<(), Error> {
            self.word(BEGIN_NODE)?;
            self.bytes(name)?;
            self.bytes(&[0])?;
            self.align()
        }

        fn property(&mut self, name: u32, value: &[u8]) -> Result<(), Error> {
            self.word(PROP)?;
            self.word(u32::try_from(value.len()).map_err(|_| Error::NoSpace)?)?;
            self.word(name)?;
            self.bytes(value)?;
            self.align()
        }

        fn modules<'a>(
            &mut self,
            strings: u32,
            base: usize,
            modules: &mut impl Iterator<Item = &'a StaticModule>,
        ) -> Result<(), Error> {
            self.node(b"tau,modules")?;
            self.property(strings, &2u32.to_be_bytes())?;
            self.property(strings + 15, &2u32.to_be_bytes())?;
            for m in modules {
                let (name, address, len) = {
                    let len = m.name.iter().position(|&c| c == 0).unwrap_or(m.name.len());
                    (
                        &m.name[..len],
                        (base + (m.offset << 12)) as u64,
                        (m.pages << 12) as u64,
                    )
                };
                if name.is_empty()
                    || name.contains(&0)
                    || len == 0
                    || address.checked_add(len).is_none()
                {
                    return Err(Error::Invalid);
                }
                self.word(BEGIN_NODE)?;
                self.bytes(b"module@")?;
                let mut leading = true;
                for shift in (0..16).rev() {
                    let digit = ((address >> (shift * 4)) & 15) as u8;
                    if digit != 0 || shift == 0 {
                        leading = false;
                    }
                    if !leading {
                        self.bytes(&[if digit < 10 {
                            b'0' + digit
                        } else {
                            b'a' + digit - 10
                        }])?;
                    }
                }
                self.bytes(&[0])?;
                self.align()?;
                // A string property includes its terminating NUL in its length.
                self.word(PROP)?;
                self.word(u32::try_from(name.len() + 1).map_err(|_| Error::Invalid)?)?;
                self.word(strings + 27)?;
                self.bytes(name)?;
                self.bytes(&[0])?;
                self.align()?;
                let mut reg = [0; 16];
                reg[..8].copy_from_slice(&address.to_be_bytes());
                reg[8..].copy_from_slice(&len.to_be_bytes());
                self.property(strings + 36, &reg)?;
                self.word(END_NODE)?;
            }
            self.word(END_NODE)
        }
    }

    /// Preserve the input tree and reservations, adding named physical byte ranges.
    /// The source must be a v17-compatible FDT. Existing /chosen properties and
    /// children are preserved; a preexisting tau,modules subtree is rejected.
    /// On error the destination is unspecified; the source is never modified.
    pub fn with_modules<'a>(
        source: &[u8],
        destination: &mut [u8],
        base: usize,
        mut modules: impl Iterator<Item = &'a StaticModule>,
    ) -> Result<usize, Error> {
        if word(source, 0)? != 0xd00d_feed || word(source, 20)? < 17 || word(source, 24)? > 17 {
            return Err(Error::Invalid);
        }
        let source = span(source, 0, word(source, 4)? as usize)?;
        let structure_offset = word(source, 8)? as usize;
        let strings_offset = word(source, 12)? as usize;
        let reserve_offset = word(source, 16)? as usize;
        let strings = span(source, strings_offset, word(source, 32)? as usize)?;
        let structure = span(source, structure_offset, word(source, 36)? as usize)?;
        if reserve_offset < 40 || reserve_offset & 7 != 0 || structure_offset & 3 != 0 {
            return Err(Error::Invalid);
        }
        let mut reserve_end = reserve_offset;
        loop {
            let entry = span(source, reserve_end, 16)?;
            reserve_end += 16;
            if entry.iter().all(|&byte| byte == 0) {
                break;
            }
        }
        // v17 block order: header, reservations, structure, strings.
        if reserve_end > structure_offset || structure_offset + structure.len() > strings_offset {
            return Err(Error::Invalid);
        }
        let names = u32::try_from(strings.len()).map_err(|_| Error::Invalid)?;
        names
            .checked_add(NAMES.len() as u32)
            .ok_or(Error::Invalid)?;
        let mut output = Writer {
            bytes: destination,
            pos: 0,
        };
        output.bytes(span(source, 0, 40)?)?;
        output.bytes(span(source, reserve_offset, reserve_end - reserve_offset)?)?;
        let new_structure = output.pos;
        let mut cursor = 0;
        let mut depth = 0usize;
        let mut root = false;
        let mut chosen = false;
        let mut in_chosen = false;
        loop {
            let start = cursor;
            let token = word(structure, cursor)?;
            cursor += 4;
            match token {
                BEGIN_NODE => {
                    let tail = structure.get(cursor..).ok_or(Error::Invalid)?;
                    let len = tail.iter().position(|&b| b == 0).ok_or(Error::Invalid)?;
                    let name = span(structure, cursor, len)?;
                    if depth == 0 {
                        if root || !name.is_empty() {
                            return Err(Error::Invalid);
                        }
                        root = true;
                    }
                    if depth == 1 && name == b"chosen" {
                        if chosen {
                            return Err(Error::Invalid);
                        }
                        chosen = true;
                        in_chosen = true;
                    }
                    if depth == 2 && in_chosen && name == b"tau,modules" {
                        return Err(Error::ExistingModules);
                    }
                    cursor = cursor.checked_add(padded(len + 1)?).ok_or(Error::Invalid)?;
                    depth += 1;
                }
                END_NODE => {
                    if depth == 0 {
                        return Err(Error::Invalid);
                    }
                    if depth == 2 && in_chosen {
                        output.modules(names, base, &mut modules)?;
                        in_chosen = false;
                    } else if depth == 1 && !chosen {
                        output.node(b"chosen")?;
                        output.modules(names, base, &mut modules)?;
                        output.word(END_NODE)?;
                    }
                    depth -= 1;
                }
                PROP => {
                    if depth == 0 {
                        return Err(Error::Invalid);
                    }
                    let len = word(structure, cursor)? as usize;
                    let name = word(structure, cursor + 4)? as usize;
                    if !strings.get(name..).ok_or(Error::Invalid)?.contains(&0) {
                        return Err(Error::Invalid);
                    }
                    cursor = cursor
                        .checked_add(8)
                        .and_then(|v| v.checked_add(padded(len).ok()?))
                        .ok_or(Error::Invalid)?;
                }
                NOP => {}
                END => {
                    if !root || depth != 0 || cursor != structure.len() {
                        return Err(Error::Invalid);
                    }
                }
                _ => return Err(Error::Invalid),
            }
            output.bytes(span(structure, start, cursor - start)?)?;
            if token == END {
                break;
            }
        }
        let new_strings = output.pos;
        output.bytes(strings)?;
        output.bytes(NAMES)?;
        output.align()?;
        let total = output.pos;
        for (at, value) in [
            (4, total),
            (8, new_structure),
            (12, new_strings),
            (16, 40),
            (20, 17),
            (24, 16),
            (32, total - new_strings),
            (36, new_strings - new_structure),
        ] {
            let value = u32::try_from(value).map_err(|_| Error::NoSpace)?;
            output
                .bytes
                .get_mut(at..at + 4)
                .ok_or(Error::NoSpace)?
                .copy_from_slice(&value.to_be_bytes());
        }
        Ok(total)
    }
}
