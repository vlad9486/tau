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
pub use self::uart::{UartIo, UartPrinter, Uart};

mod dwmmc;

use core::cell::UnsafeCell;
use core::pin::pin;
use core::task::{Context, Poll};

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

    tau::Ubi::map(NonZeroUsize::new(info), 0x0100_1000, info_pages).unwrap_or_default();
    let raw = unsafe {
        slice::from_raw_parts(
            0x0100_1000 as *mut u32,
            (info_pages << 12) / size_of::<u32>(),
        )
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

    let Some(uart_config) = dtb.iter().find_map(|(props, path)| {
        (path[1] == "soc" && path[2].starts_with("serial")).then_some(props)
    }) else {
        tau::Ubi::respond(inv.inv, 1, []);
    };

    let Some([addr_hi, addr_lo, _, _]) = uart_config.find_int(|name| name == "reg") else {
        tau::Ubi::respond(inv.inv, 1, []);
    };
    let addr = ((addr_hi.to_be() as usize) << 32) + (addr_lo.to_be() as usize);

    let reg_width = uart_config
        .find_int(|name| name == "reg-io-width")
        .and_then(|x| x.first().copied().map(u32::to_be))
        .unwrap_or(1);
    let Some(uart_int) = uart_config
        .find_int(|name| name == "interrupts")
        .and_then(|x| x.first().copied().map(u32::to_be))
    else {
        tau::Ubi::respond(inv.inv, 1, []);
    };
    let uart_int = InterruptNumber::new(uart_int);

    tau::Ubi::map(NonZeroUsize::new(addr), 0x0100_0000, 1).unwrap_or_default();
    let uart = if reg_width == 1 {
        unsafe { &*(0x0100_0000 as *mut Uart<false, u8>) }.init(115200) as &dyn UartIo
    } else if reg_width == 4 {
        unsafe { &*(0x0100_0000 as *mut Uart<true, u32>) }.init(115200) as &dyn UartIo
    } else {
        tau::Ubi::respond(inv.inv, 1, []);
    };

    let Some(plic_config) = dtb.iter().find_map(|(props, path)| {
        (path[1] == "soc" && path[2].starts_with("plic")).then_some(props)
    }) else {
        tau::Ubi::respond(inv.inv, 1, []);
    };

    let Some([addr_hi, addr_lo, _, _]) = plic_config.find_int(|name| name == "reg") else {
        tau::Ubi::respond(inv.inv, 1, []);
    };
    let addr = ((addr_hi.to_be() as usize) << 32) + (addr_lo.to_be() as usize);

    tau::Ubi::map(NonZeroUsize::new(addr), 0x0200_0000, 3).unwrap_or_default();
    let plic = unsafe { &*(0x0200_0000 as *mut Plic) };

    let context_id = cpus
        .iter()
        .take(hart_id + 1)
        .map(|cpu| usize::from(cpu.contexts))
        .sum::<usize>()
        - 1;
    let context_addr = addr + 0x0020_0000 + context_id * 0x1000;
    tau::Ubi::map(NonZeroUsize::new(context_addr), 0x0200_3000, 1).unwrap_or_default();
    let plic_tc = unsafe { &*(0x0200_3000 as *mut PlicThresholdClaim) };

    // setup plic
    {
        // should do only once
        plic.set_priority(&uart_int, InterruptPriority::_1);
    }
    plic.enable(context_id, &uart_int);
    plic_tc.set_threshold(InterruptPriority::_0);

    #[pin_project::pin_project]
    struct Drivers<'a, Dwmmc> {
        #[pin]
        dwmmc: Option<Dwmmc>,
        dwmmc_int: &'a [u32],
    }

    let interrupt = UnsafeCell::new(None);
    let runtime = Runtime {
        plic_claim: plic_tc,
        uart,
        interrupt: &interrupt,
    };

    let mut drivers = Drivers {
        dwmmc: None,
        dwmmc_int: &[],
    };
    if let Some(config) = dtb.iter().find_map(|(props, path)| {
        (path[1] == "soc" && path[2].starts_with("sdio1@")).then_some(props)
    }) {
        let int = config.find_int(|name| name == "interrupts");
        drivers.dwmmc = Some(dwmmc::run(runtime, config, 0x0201_0000));

        if let Some(int) = int {
            drivers.dwmmc_int = int;
            for int in int {
                let int = int.to_be();
                plic.set_priority(&InterruptNumber::new(int), InterruptPriority::_1);
                plic.enable(context_id, &InterruptNumber::new(int));
            }
        }
    }

    let waker = noop_waker::noop_waker();
    let mut cx = Context::from_waker(&waker);

    let drivers = pin!(drivers);
    let mut drivers = drivers.project();
    if let Some(fut) = drivers.dwmmc.as_mut().as_pin_mut() {
        let _ = fut.poll(&mut cx);
    }

    'main: loop {
        tau::Ubi::wait();
        if let Some(int) = plic_tc.next() {
            let is_uart = int == uart_int;
            if is_uart {
                while let Some(c) = uart.rx() {
                    uart.tx(c);
                    if c == b'\r' {
                        uart.tx(b'\n');
                        plic_tc.complete(int);
                        break 'main;
                    }
                }
            } else {
                if int.belongs(&*drivers.dwmmc_int) {
                    if let Some(fut) = drivers.dwmmc.as_mut().as_pin_mut() {
                        unsafe {
                            interrupt.get().write(Some(int));
                        }
                        if let Poll::Ready(res) = fut.poll(&mut cx) {
                            drivers.dwmmc.set(None);
                            if let Err(err) = res {
                                runtime.error(format_args!("{err}"));
                            }
                        }
                        continue;
                    }
                }
            }
            plic_tc.complete(int);
        }
    }

    tau::Ubi::respond(inv.inv, 0, [])
}
