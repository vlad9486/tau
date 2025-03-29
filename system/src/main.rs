#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(tau::tester::test_runner)]

mod register;

pub mod asm;

pub mod driver;
use self::driver::Runtime;

mod plic;
pub use self::plic::{Plic, PlicThresholdClaim, InterruptPriority, InterruptNumber};

mod uart;
mod sdio;

use core::pin;

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
    mapped_regions: &[tau::MappedRegion::stack(0x10000)],
};

// TODO: proper heap
const DTB_ADDR: usize = 0x0100_0000;
const PLIC_ADDR: usize = 0x0200_0000;
const PLIC_CONTEXTS_ADDR: usize = 0x0200_3000;

const UART_ADDR: usize = 0x0210_0000;
const SDIO_ADDR: usize = 0x0210_1000;

#[cold]
extern "C" fn main(
    a0: usize,
    hart_id: usize,
    info: usize,
    info_pages: usize,
    _: usize,
    _: usize,
) -> ! {
    use core::{num::NonZeroUsize, slice};

    let Ok(inv) = tau::Inv::decode(a0) else {
        tau::Ubi::exit([1]);
    };

    tau::Ubi::map(NonZeroUsize::new(info), DTB_ADDR, info_pages).unwrap_or_default();
    let raw = unsafe {
        slice::from_raw_parts(DTB_ADDR as *mut u32, (info_pages << 12) / size_of::<u32>())
    };
    let Ok((dtb, _)) = tau::Dtb::new(raw) else {
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

    let Some(plic_config) = dtb.iter().find_map(|(props, path)| {
        (path[1] == "soc" && path[2].starts_with("plic")).then_some(props)
    }) else {
        tau::Ubi::respond(inv.inv, 1, []);
    };

    let Some([addr_hi, addr_lo, _, _]) = plic_config.find_int(|name| name == "reg") else {
        tau::Ubi::respond(inv.inv, 1, []);
    };
    let addr = ((addr_hi.to_be() as usize) << 32) + (addr_lo.to_be() as usize);

    tau::Ubi::map(NonZeroUsize::new(addr), PLIC_ADDR, 3).unwrap_or_default();
    let plic = unsafe { &*(PLIC_ADDR as *mut Plic) };

    let context_id = cpus
        .iter()
        .take(hart_id + 1)
        .map(|cpu| usize::from(cpu.contexts))
        .sum::<usize>()
        - 1;
    let context_addr = addr + 0x0020_0000 + context_id * 0x1000;
    tau::Ubi::map(NonZeroUsize::new(context_addr), PLIC_CONTEXTS_ADDR, 1).unwrap_or_default();
    let plic_tc = unsafe { &*(PLIC_CONTEXTS_ADDR as *mut PlicThresholdClaim) };
    plic_tc.set_threshold(InterruptPriority::_0);

    let rt = Runtime::new(plic_tc);
    let drivers = driver::drivers(dtb, &rt, plic, context_id, UART_ADDR, SDIO_ADDR);
    pin::pin!(drivers).run(&rt);

    tau::Ubi::respond(inv.inv, 0, [])
}
