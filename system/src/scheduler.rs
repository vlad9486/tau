use core::cell::UnsafeCell;
use core::{hint, fmt, num::NonZeroUsize, time::Duration};

use thiserror_no_std::Error;

use super::plic::{PlicPriority, PlicEnable, PlicCtx, InterruptNumber, InterruptId, InterruptPriority};
use super::{uart, sdio, user};

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
    pub deadline: [Option<Deadline>; 8],
    freq: u128,
}

#[derive(Clone, Copy)]
pub struct Deadline {
    val: NonZeroUsize,
    issuer: u8,
}

impl Shared {
    pub fn sleep(&mut self, issuer: u8, delay: Duration) {
        let tick = ((delay.as_nanos() * self.freq) / 1_000_000_000) as usize;
        let val = unsafe { NonZeroUsize::new_unchecked(tau::asm::read_time().wrapping_add(tick)) };
        let d = Deadline { val, issuer };
        *self
            .deadline
            .iter_mut()
            .find(|d| d.is_none())
            .expect("too many timers") = Some(d);
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
        let shared = UnsafeCell::new(Shared {
            uart_buffer: uart::Buffer::default(),
            sdio_task: None,
            sdio_done: None,
            terminate: false,
            deadline: [None; 8],
            // TODO: take from dts
            freq: 4_000_000_u128,
        });
        let mut user = user::State::new(&shared);

        let shared = unsafe { &mut *shared.get() };

        user.step();
        if let Some(driver) = self.uart.as_mut() {
            driver.state.handle(shared, tau::Event::Timeout);
        }
        if let Some(driver) = self.sdio.as_mut() {
            driver.state.handle(shared, tau::Event::Timeout);
        }

        while !(shared.terminate && shared.uart_buffer.is_empty()) {
            let deadline = shared
                .deadline
                .iter()
                .filter_map(|x| *x)
                .map(|d| d.val)
                .min();
            let mut event = Some(tau::Ubi::wait(deadline));

            while let Some(event) = tau::event_with(&mut event, plic.next()) {
                match &event {
                    tau::Event::Signal { .. } => continue,
                    tau::Event::Interrupt(int) => {
                        handle(&mut self.uart, shared, int);
                        handle(&mut self.sdio, shared, int);

                        if shared.sdio_done.is_some() {
                            user.step();
                        }
                    }
                    tau::Event::Timeout => {
                        let now = tau::asm::read_time();
                        let mut issuers = [0; 8];
                        let mut it = issuers.iter_mut();
                        for d in &mut shared.deadline {
                            if let Some(dl) = d {
                                if dl.val.get() <= now {
                                    *it.next().expect("cannot fail") = dl.issuer;
                                    *d = None;
                                }
                            }
                        }
                        for issuer in issuers {
                            match issuer {
                                1 => {
                                    if let Some(driver) = self.sdio.as_mut() {
                                        driver.state.handle(shared, tau::Event::Timeout);
                                    }
                                }
                                2 => user.step(),
                                _ => (),
                            }
                        }
                    }
                }

                if let tau::Event::Interrupt(int) = event {
                    plic.complete(int);
                }
            }

            if !shared.uart_buffer.is_empty() {
                if let Some(driver) = self.uart.as_mut() {
                    driver.state.handle(shared, tau::Event::Timeout);
                }
            }
        }
    }
}
