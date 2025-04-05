use core::{hint, fmt, num::NonZeroUsize, time::Duration};

use alloc::boxed::Box;

use thiserror_no_std::Error;

use super::plic::{PlicPriority, PlicEnable, PlicCtx, InterruptNumber, InterruptId, InterruptPriority};
use super::{uart, sdio};

pub trait DriverState
where
    Self: Sized,
{
    fn handle(&mut self, shared: &mut Shared, event: tau::Event<u32>);
}

pub struct Shared {
    pub uart_buffer: uart::Buffer,
    pub sdio_task: Option<sdio::Task>,
    pub sdio_done: Option<sdio::Task>,
    pub terminate: bool,
    pub deadline: Option<NonZeroUsize>,
    freq: u128,
}

impl Shared {
    pub fn sleep(&mut self, delay: Duration) {
        let tick = ((delay.as_nanos() * self.freq) / 1_000_000_000) as usize;
        let d = tau::asm::read_time().wrapping_add(tick);
        self.deadline = NonZeroUsize::new(d);
    }

    pub fn write(&mut self, args: fmt::Arguments<'_>) {
        let nanos = ((tau::asm::read_time() as u128) * 1_000_000_000) / self.freq;
        self.uart_buffer.write(nanos, args);
    }
}

#[derive(Debug, Error)]
#[error("timeout")]
pub struct Timeout;

pub fn spin(mut timeout: u32, cond: impl Fn() -> bool) -> Result<(), Timeout> {
    while !cond() {
        hint::spin_loop();
        timeout -= 1;
        if timeout == 0 {
            return Err(Timeout);
        }
    }

    Ok(())
}

struct Driver<S> {
    state: S,
    int: [u32; 16],
}

impl<S> Driver<S> {
    pub fn parse_dtb<'dtb, F>(
        config: tau::DtbProps<'dtb>,
        plic: &PlicPriority,
        plic_e: &PlicEnable,
        context_id: usize,
        factory: F,
    ) -> Option<Self>
    where
        F: FnOnce(tau::DtbProps<'dtb>) -> Option<S>,
    {
        let sl = config.find_int(|name| name == "interrupts").unwrap_or(&[]);
        let mut int = [u32::MAX; 16];
        for (num, i) in sl.iter().zip(int.iter_mut()) {
            let num = num.to_be();
            *i = num;
            let num = InterruptNumber::new(num);
            plic.set_priority(&num, InterruptPriority::_1);
            plic_e.enable(context_id, &num);
        }
        factory(config).map(|state| Driver { int, state })
    }
}

fn handle<S>(driver: &mut Option<Driver<S>>, shared: &mut Shared, id: &InterruptId)
where
    S: DriverState,
{
    if let Some(driver) = driver {
        let num = id.as_ref().get();
        if driver.int.contains(&num) {
            driver.state.handle(shared, tau::Event::Interrupt(num))
        }
    }
}

pub struct Tasks {
    uart: Option<Driver<uart::State>>,
    sdio: Option<Driver<sdio::State>>,
}

impl Tasks {
    pub fn new(
        dtb: tau::Dtb<'_>,
        plic: &PlicPriority,
        plic_e: &PlicEnable,
        context_id: usize,
    ) -> Tasks {
        let uart = dtb
            .iter()
            .find(|(_, path)| (path[1] == "soc" && path[2].starts_with("serial@")))
            .and_then(|(config, _)| {
                Driver::parse_dtb(config, plic, plic_e, context_id, |config| {
                    Some(uart::State::new(uart::Config::parse(config, 115200)?))
                })
            });
        let sdio = dtb
            .iter()
            .find(|(_, path)| (path[1] == "soc" && path[2].starts_with("sdio1@")))
            .and_then(|(config, _)| {
                Driver::parse_dtb(config, plic, plic_e, context_id, |config| {
                    sdio::State::new(config)
                })
            });
        Tasks { uart, sdio }
    }

    pub fn run(&mut self, plic: &PlicCtx) {
        // TODO: move in user task
        // TODO: allocator for DMA
        let phys = 0x7000_1000;
        let page =
            Box::<[u8], _>::new_uninit_slice_in(0x1000, tau::Area::new(phys as usize, 0x1000));

        let mut shared = Shared {
            uart_buffer: uart::Buffer::default(),
            sdio_task: Some(sdio::Task::Read { page: 0x400, phys }),
            sdio_done: None,
            terminate: false,
            deadline: None,
            // TODO: take from dts
            freq: 4_000_000_u128,
        };

        if let Some(driver) = self.uart.as_mut() {
            driver.state.handle(&mut shared, tau::Event::Timeout);
        }
        if let Some(driver) = self.sdio.as_mut() {
            driver.state.handle(&mut shared, tau::Event::Timeout);
        }

        while !(shared.terminate && shared.uart_buffer.is_empty()) {
            // TODO: proper time wheel
            let mut event = Some(tau::Ubi::wait(shared.deadline.take()));

            while let Some(event) = tau::event_with(&mut event, plic.next()) {
                match &event {
                    tau::Event::Signal { .. } => continue,
                    tau::Event::Interrupt(int) => {
                        handle(&mut self.uart, &mut shared, int);
                        handle(&mut self.sdio, &mut shared, int);
                    }
                    tau::Event::Timeout => {
                        if let Some(driver) = self.sdio.as_mut() {
                            driver.state.handle(&mut shared, tau::Event::Timeout);
                        }
                    }
                }

                if let tau::Event::Interrupt(int) = event {
                    plic.complete(int);
                }
            }

            // TODO: move in user task
            if shared.sdio_done.take().is_some() {
                let dma_data = unsafe { page.assume_init_ref() };
                for chunk in dma_data.chunks(0x10) {
                    let &[a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = chunk else {
                        break;
                    };
                    shared.write(format_args!(
                        "\
                        {a:02x} {b:02x} {c:02x} {d:02x} {e:02x} {f:02x} {g:02x} {h:02x} \
                        {i:02x} {j:02x} {k:02x} {l:02x} {m:02x} {n:02x} {o:02x} {p:02x}"
                    ));
                }
            }

            if !shared.uart_buffer.is_empty() {
                if let Some(driver) = self.uart.as_mut() {
                    driver.state.handle(&mut shared, tau::Event::Timeout);
                }
            }
        }
    }
}
