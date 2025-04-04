#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(tau::tester::test_runner)]
#![feature(allocator_api)]
#![feature(new_zeroed_alloc)]
#![feature(maybe_uninit_slice)]

extern crate alloc;

mod register;

mod driver;
mod plic;

mod uart;
mod sdio;

#[cfg(not(test))]
#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo) -> ! {
    tau::Ubi::exit([1]);
}

#[unsafe(no_mangle)]
static MANIFEST: tau::Manifest = tau::Manifest {
    this: tau::ModuleId {
        version: (0, 1),
        name: *b"system\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
    },
    entry: main,
    dependencies: &[tau::ModuleId {
        version: (0, 1),
        name: *b"some other module\0\0\0",
    }],
    mapped_regions: &[tau::MappedRegion::stack(0x20000)],
};

#[cold]
extern "C" fn main(
    a0: usize,
    hart_id: usize,
    info: usize,
    info_pages: usize,
    _: usize,
    _: usize,
) -> ! {
    use alloc::boxed::Box;

    let Ok(inv) = tau::Inv::decode(a0) else {
        tau::Ubi::exit([1]);
    };

    let len = info_pages << 12;
    let dtb_alloc = tau::Area::new(info, len);
    let raw = unsafe {
        Box::<[u32], _>::new_uninit_slice_in(len / size_of::<u32>(), dtb_alloc).assume_init()
    };
    let Ok((dtb, _)) = tau::Dtb::new(&raw) else {
        tau::Ubi::respond(inv.inv, 1, []);
    };

    #[derive(Clone, Copy, Debug)]
    struct CpuInfo {
        id: u16,
        contexts: u8,
    }

    // TODO: allocate cpu map in heap in vector
    let mut cpus = [CpuInfo { id: 0, contexts: 0 }; 64];
    let cpus_it = dtb
        .iter()
        .filter(|(_, path)| path[1] == "cpus" && path[2].starts_with("cpu@") && path.len() == 3)
        .zip(cpus.iter_mut());
    for ((props, _), cpu) in cpus_it {
        let id = props
            .find_int(|name| name == "reg")
            .and_then(|x| x.first().copied().map(u32::to_be))
            .unwrap_or(0xffff);
        if props.find_str(|name| name == "status") == Some("okay") {
            cpu.contexts = 2;
        } else {
            cpu.contexts = 1;
        }
        cpu.id = id as u16;
    }

    let context_id = cpus
        .iter()
        .take(hart_id + 1)
        .map(|cpu| usize::from(cpu.contexts))
        .sum::<usize>()
        - 1;

    let Some(plic_config) = dtb.iter().find_map(|(props, path)| {
        (path[1] == "soc" && path[2].starts_with("plic")).then_some(props)
    }) else {
        tau::Ubi::respond(inv.inv, 1, []);
    };

    let Some([addr_hi, addr_lo, _, _]) = plic_config.find_int(|name| name == "reg") else {
        tau::Ubi::respond(inv.inv, 1, []);
    };
    let addr = ((addr_hi.to_be() as usize) << 32) + (addr_lo.to_be() as usize);

    let plic = unsafe {
        Box::<plic::PlicPriority, _>::new_uninit_in(tau::Area::new(addr, 0x2000)).assume_init()
    };

    let plic_e = unsafe {
        let a = tau::Area::new(addr + plic::enable_offset(context_id), 0x1000);
        Box::<plic::PlicEnable, _>::new_uninit_in(a).assume_init()
    };

    let plic_ctx = unsafe {
        let a = tau::Area::new(addr + plic::context_offset(context_id), 0x1000);
        Box::<plic::PlicCtx, _>::new_uninit_in(a).assume_init()
    };
    plic_ctx.set_threshold(plic::InterruptPriority::_0);

    let mut drivers = driver::drivers(dtb, &plic, &plic_e, context_id);
    drivers.run(&plic_ctx);

    tau::Ubi::respond(inv.inv, 0, [])
}
