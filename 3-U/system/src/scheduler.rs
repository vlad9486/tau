// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::cell::UnsafeCell;
use core::{hint, fmt, num::NonZeroUsize, time::Duration};

use smallvec::SmallVec;

use super::plic::{PlicPriority, PlicEnable, PlicCtx, InterruptNumber, InterruptId, InterruptPriority};
use super::{uart, sdio, ethernet, display, user};

pub trait DriverState
where
    Self: Sized,
{
    fn handle(&mut self, shared: &mut Shared, event: tau::Event);
}

pub struct Shared {
    pub uart_out: uart::Buffer,
    pub uart_in: uart::Buffer,
    pub sdio_task: Option<sdio::Task>,
    pub sdio_done: Option<sdio::Task>,
    // One producer and one outstanding TX per port. Consume done before reuse.
    pub ethernet_tx_task: [Option<ethernet::TxTask>; 2],
    pub ethernet_tx_done: [Option<ethernet::TxDone>; 2],
    pub ethernet_rx_task: [Option<ethernet::RxTask>; 2],
    pub ethernet_rx_done: [Option<ethernet::RxDone>; 2],
    pub display_task: Option<display::Task>,
    pub display_done: Option<Result<(), display::Error>>,
    pub terminate: bool,
    deadline: [Option<Deadline>; 8],
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
        let nanos = self.nanos(tau::asm::read_time());
        self.uart_out.write(nanos, args);
    }

    pub fn nanos(&self, ticks: usize) -> u128 {
        ((ticks as u128) * 1_000_000_000) / self.freq
    }
}

#[derive(Debug)]
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
            let Some(num) = InterruptNumber::new(num) else {
                // the number is zero, should not have such number in the dtb
                continue;
            };
            plic.set_priority(&num, InterruptPriority::_1);
            plic_e.enable(context_id, &num);
        }
        factory(config).map(|state| Driver { int, state })
    }
}

fn handle<'a, S>(
    driver: impl IntoIterator<Item = &'a mut Driver<S>>,
    shared: &mut Shared,
    id: &InterruptId,
) where
    S: DriverState + 'a,
{
    for driver in driver {
        let num = id.as_ref().get();
        if driver.int.contains(&num) {
            let arg = num as u16;
            driver.state.handle(shared, tau::Event::Notified { arg })
        }
    }
}

pub struct Tasks {
    uart: Option<Driver<uart::State>>,
    sdio: Option<Driver<sdio::State>>,
    ethernet: SmallVec<[Driver<ethernet::State>; 4]>,
    display: Option<display::State>,
    freq: u128,
}

impl Tasks {
    pub fn new(
        dtb: &tau::Dtb<'_>,
        reservations: tau::DtbRsv<'_>,
        plic: &PlicPriority,
        plic_e: &PlicEnable,
        context_id: usize,
    ) -> Tasks {
        let freq = dtb
            .iter()
            .find_map(|(props, path)| {
                if path.len() != 2 || path[1] != "cpus" {
                    return None;
                }
                props
                    .find_int(|name| name == "timebase-frequency")?
                    .first()
                    .copied()
                    .map(u32::to_be)
                    .filter(|&freq| freq != 0)
            })
            .map(u128::from)
            .unwrap_or(4_000_000);
        let uart = dtb
            .iter()
            .find(|(_, path)| path[1] == "soc" && path[2].starts_with("serial@"))
            .and_then(|(config, _)| {
                Driver::parse_dtb(config, plic, plic_e, context_id, |config| {
                    Some(uart::State::new(uart::Config::parse(config, 115200)?))
                })
            });
        let sdio = dtb
            .iter()
            .find(|(_, path)| path[1] == "soc" && path[2].starts_with("sdio1@"))
            .and_then(|(config, _)| {
                Driver::parse_dtb(config, plic, plic_e, context_id, |config| {
                    sdio::State::new(config)
                })
            });
        let ethernet = dtb
            .iter()
            .filter(|(_, path)| path[1] == "soc" && path[2].starts_with("ethernet@"))
            .filter_map(|(config, _)| {
                Driver::parse_dtb(config, plic, plic_e, context_id, |config| {
                    ethernet::State::new(config)
                })
            })
            .collect();
        let display = display::State::new(dtb, reservations);

        Tasks {
            uart,
            sdio,
            ethernet,
            // Fixed framebuffer scanout uses polling, so no display IRQ is
            // enabled in the PLIC. Timer issuer 5 drives initialization/drawing.
            display,
            freq,
        }
    }

    pub fn run(&mut self, plic: &PlicCtx, dummy_slot: u16, hart_id: usize) {
        let shared = UnsafeCell::new(Shared {
            uart_out: uart::Buffer::default(),
            uart_in: uart::Buffer::default(),
            sdio_task: None,
            sdio_done: None,
            ethernet_tx_task: [None; 2],
            ethernet_tx_done: [None; 2],
            ethernet_rx_task: [None; 2],
            ethernet_rx_done: [None; 2],
            display_task: None,
            display_done: None,
            terminate: false,
            deadline: [None; 8],
            freq: self.freq,
        });
        let mut user = user::State::new(
            &shared,
            self.sdio.is_some(),
            !self.ethernet.is_empty(),
            self.display.is_some(),
            dummy_slot,
        );

        let shared = unsafe { &mut *shared.get() };

        if let Some(driver) = self.uart.as_mut() {
            driver.state.handle(shared, tau::Event::Timeout);
        }
        user.step();
        if !shared.uart_out.is_empty()
            && let Some(driver) = self.uart.as_mut()
        {
            driver.state.handle(shared, tau::Event::Timeout);
        }
        if let Some(driver) = self.sdio.as_mut() {
            driver.state.handle(shared, tau::Event::Timeout);
        }
        for driver in self.ethernet.as_mut() {
            driver.state.handle(shared, tau::Event::Timeout);
        }
        if let Some(driver) = self.display.as_mut() {
            driver.handle(shared, tau::Event::Timeout);
        } else {
            shared.write(format_args!("no display found\n"));
        }
        if shared.display_done.is_some() {
            user.step();
        }
        if !shared.uart_out.is_empty()
            && let Some(driver) = self.uart.as_mut()
        {
            driver.state.handle(shared, tau::Event::Timeout);
        }

        while !(shared.terminate && shared.uart_out.is_empty()) {
            let deadline = shared
                .deadline
                .iter()
                .filter_map(|x| *x)
                .map(|d| d.val)
                .min();

            match tau::Ubi::wait(&crate::WAIT_SLOTS[hart_id], deadline) {
                Ok(tau::Event::Notified { .. }) => {
                    while let Some(id) = plic.next() {
                        handle(self.uart.as_mut(), shared, &id);
                        handle(self.sdio.as_mut(), shared, &id);
                        handle(self.ethernet.iter_mut(), shared, &id);

                        if shared.sdio_done.is_some() || !shared.uart_in.is_empty() {
                            user.step();
                        }

                        plic.complete(id);
                    }
                }
                Ok(tau::Event::Timeout) => {
                    let now = tau::asm::read_time();
                    let mut issuers = [0; 8];
                    let mut it = issuers.iter_mut();
                    for d in &mut shared.deadline {
                        if let Some(dl) = d
                            && dl.val.get() <= now
                        {
                            *it.next().expect("cannot fail") = dl.issuer;
                            *d = None;
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
                            3 | 4 => {
                                for driver in self.ethernet.as_mut() {
                                    if driver.state.timer_issuer() == issuer {
                                        driver.state.handle(shared, tau::Event::Timeout);
                                    }
                                }
                            }
                            5 => {
                                if let Some(driver) = self.display.as_mut() {
                                    driver.handle(shared, tau::Event::Timeout);
                                }
                            }
                            _ => (),
                        }
                    }
                }
                Err(_) => (),
            }

            // A completion can immediately cause user.rs to submit another
            // Ethernet or display task. Drain exchanges before sleeping, including
            // synchronous errors on absent/not-ready ports. Bound the work so
            // an unconsumed completion cannot starve interrupt processing.
            for _ in 0..2 {
                if let Some(driver) = self.display.as_mut() {
                    driver.submit(shared);
                } else if shared.display_done.is_none() && shared.display_task.take().is_some() {
                    shared.display_done = Some(Err(display::Error::Failed));
                }
                for port in 0..2 {
                    if let Some(driver) = self
                        .ethernet
                        .iter_mut()
                        .find(|driver| driver.state.timer_issuer() == 3 + port as u8)
                    {
                        driver.state.submit(shared);
                    } else {
                        if shared.ethernet_tx_done[port].is_none()
                            && shared.ethernet_tx_task[port].take().is_some()
                        {
                            shared.ethernet_tx_done[port] = Some(Err(ethernet::TxError::Failed));
                        }
                        if shared.ethernet_rx_done[port].is_none()
                            && shared.ethernet_rx_task[port].take().is_some()
                        {
                            shared.ethernet_rx_done[port] = Some(Err(ethernet::RxError::Failed));
                        }
                    }
                }
                if shared.ethernet_tx_done.iter().any(Option::is_some)
                    || shared.ethernet_rx_done.iter().any(Option::is_some)
                    || shared.display_done.is_some()
                {
                    user.step();
                } else {
                    break;
                }
            }

            if !shared.uart_out.is_empty()
                && let Some(driver) = self.uart.as_mut()
            {
                driver.state.handle(shared, tau::Event::Timeout);
            }
        }
    }
}
