#![no_std]
#![no_main]
#![cfg_attr(
    feature = "nightly",
    feature(custom_test_frameworks),
    test_runner(tau::tester::test_runner)
)]

extern crate alloc;

mod register;

mod scheduler;
mod plic;

mod uart;
mod sdio;
mod ethernet;
mod user;

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
    let Ok(tau::Event::Invocation { inv, .. }) = tau::Event::decode(a0) else {
        tau::Ubi::exit([1]);
    };

    let len = info_pages << 12;
    let raw = tau::Area::new(info, len).sl();
    let Ok((dtb, _)) = tau::Dtb::new(raw) else {
        tau::Ubi::respond(inv, 1, []);
    };

    let Some((cpu_props, cpu_path)) = dtb
        .iter()
        .filter(|(_, path)| path[1] == "cpus" && path[2].starts_with("cpu@") && path.len() == 3)
        .find_map(|(props, path)| {
            let reg = props.find_int(|name| name.starts_with("reg"))?;
            let reg = reg.first().copied().map(u32::to_be)?;
            (tau::to_size(reg) == hart_id).then_some((props, path))
        })
    else {
        tau::Ubi::respond(inv, 2, []);
    };
    let Some((cpu_interrupt_props, _)) = dtb.iter().find(|(_, path)| {
        path.len() == 4 && path[2] == cpu_path[2] && path[3].starts_with("interrupt-controller")
    }) else {
        tau::Ubi::respond(inv, 3, []);
    };
    let Some(handle) = cpu_interrupt_props
        .find_int(|name| name.starts_with("phandle"))
        .and_then(|x| x.first().copied().map(u32::to_be))
    else {
        tau::Ubi::respond(inv, 4, []);
    };

    // STATUS: can use it
    let _ = (cpu_props, cpu_interrupt_props);

    let Some(plic_config) = dtb.iter().find_map(|(props, path)| {
        (path[1] == "soc" && path[2].starts_with("plic")).then_some(props)
    }) else {
        tau::Ubi::respond(inv, 5, []);
    };

    let Some(ie) = plic_config.find_int(|name| name.starts_with("interrupts-extended")) else {
        tau::Ubi::respond(inv, 6, []);
    };
    let Some(context_id) = ie
        .chunks(2)
        .position(|sl| sl[0].to_be() == handle && sl[1].to_be() == 9)
    else {
        tau::Ubi::respond(inv, 7, []);
    };

    let Some(plic_area) = plic_config.find_reg() else {
        tau::Ubi::respond(inv, 8, []);
    };

    let plic = tau::Area::new(plic_area.base, 0x2000).r();

    let plic_e = tau::Area::new(plic_area.base + plic::enable_offset(context_id), 0x1000).r();

    let plic_ctx = tau::Area::new(plic_area.base + plic::context_offset(context_id), 0x1000)
        .r::<plic::PlicCtx>();
    plic_ctx.set_threshold(plic::InterruptPriority::_0);

    scheduler::Tasks::new(&dtb, plic, plic_e, context_id).run(plic_ctx);

    tau::Ubi::respond(inv, 0, [])
}
