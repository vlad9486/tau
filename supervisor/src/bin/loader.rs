#![no_std]
#![no_main]
#![cfg_attr(
    feature = "nightly",
    feature(custom_test_frameworks),
    test_runner(tau::tester::test_runner),
    feature(strict_provenance_lints),
    warn(fuzzy_provenance_casts)
)]

use core::{arch, cell::UnsafeCell, hint, mem::MaybeUninit, slice};

use supervisor::{
    llfree::{Error, Flags, Init, LLFree},
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

        match init_memory(hart_id, dtb_addr.addr(), dtb, st, ro, hp) {
            Ok((satp, cores, frames)) => {
                sbi::Printer.ch(*b"success\r\n");
                (satp, opaque, size.div_ceil(0x1000), cores, frames)
            }
            Err(_err) => {
                sbi::Printer.ch(*b"failed\r\n");
                loop {
                    hint::spin_loop();
                }
            }
        }
    } else {
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
    // sbi::set_timer(cpu::csrr!("time") + 0x10000000).unwrap_or_default();
}

fn init_memory(
    hart_id: usize,
    dtb_addr: usize,
    dtb: tau::Dtb<'_>,
    st: *mut MaybeUninit<[usize; 512]>,
    ro: *mut MaybeUninit<[usize; 512]>,
    hp: *mut MaybeUninit<[usize; 512]>,
) -> Result<(usize, usize, usize), Error> {
    use tau::loader::{
        SUPERVISOR_OFFSET, SUPERVISOR_SIZE, SYSTEM_OFFSET, SYSTEM_SIZE, HEAP_START, HEAP_END,
    };

    let mut cores = 0;
    let mut memory_size = 0;
    for (props, path) in dtb.iter() {
        if path.len() == 3 && path[1] == "cpus" && path[2].starts_with("cpu@") {
            cores += usize::from(props.find_str(|name| name == "status") == Some("okay"));
        } else if let Some([_, _, size_hi, size_lo]) = props.find_int(|name| name == "reg")
            && path.len() == 2
            && path[1].starts_with("memory@")
        {
            memory_size = ((size_hi.to_be() as usize) << 32) + (size_lo.to_be() as usize);
        }
    }
    if cores < 2 || memory_size < 0x1000000 * cores {
        return Err(Error::Memory);
    }
    let frames = memory_size >> 12;

    let ptr = unsafe { st.add(HEAP_END) };
    let (pages, metadata) = LLFree::create_metadata(cores, frames, true, ptr);
    let allocator = LLFree::new(Init::AllocAll, metadata)?;

    // beginning of free memory
    // `0x200` is opensbi size
    let mut frame = 0x200 + HEAP_END + pages;
    while frame & 0o777 != 0 {
        let core = ((frame >> 10) + hart_id) % cores;
        allocator.put(core, frame, Flags::o(0))?;
        frame += 1;
    }
    for big_frame in (frame / 0o1000)..(frames / 0o1000) {
        let frame = big_frame << 9;
        // TODO: handle reserved regions properly
        if frame == dtb_addr >> 12 {
            continue;
        }
        let core = ((frame >> 10) + hart_id) % cores;
        allocator.put(core, frame, Flags::o(9))?;
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
    // 0xffff_ffc0_0020_0000 .. 0xffff_ffc0_0040_0000 (2 MiB, per thread, registers)
    // 0xffff_ffc0_0040_0000 .. 0xffff_ffc0_0060_0000 (2 MiB, scheduler)
    // 0xffff_ffc0_0060_0000 .. 0xffff_ffc0_006e_0000 (896 kiB, global, supervisor image)
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

    Ok(((vmem::SV39 << 60) | (root_table >> 12), cores, frames))
}
