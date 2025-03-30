use core::{
    cell::UnsafeCell,
    fmt::{self, Write as _},
    future, hint,
    num::NonZeroUsize,
    pin::Pin,
    task::{Context, Poll},
};

use thiserror_no_std::Error;

use super::plic::{Plic, PlicThresholdClaim, InterruptId, InterruptNumber, InterruptPriority};
use super::{shell, uart, sdio};

pub struct Runtime {
    plic: &'static PlicThresholdClaim,
    event: UnsafeCell<Option<tau::Event<InterruptId>>>,
    shared: UnsafeCell<Shared>,
    log_level: LogLevel,
}

#[derive(Default)]
pub struct Shared {
    pub uart_buffer: uart::Buffer,
    pub terminate: bool,
    pub sdio_task: sdio::Task,
}

#[repr(u8)]
#[derive(Clone, Copy)]
pub enum LogLevel {
    Error = 0,
    Info = 2,
    Debug = 3,
}

impl Runtime {
    pub fn new(plic: &'static PlicThresholdClaim, log_level: LogLevel) -> Self {
        Runtime {
            plic,
            event: UnsafeCell::new(None),
            shared: UnsafeCell::new(Shared::default()),
            log_level,
        }
    }
}

impl Runtime {
    fn put(&self, event: tau::Event<InterruptId>) {
        unsafe {
            self.event.get().write(Some(event));
        }
    }

    fn take(&self) -> Option<tau::Event<InterruptId>> {
        unsafe { &mut *self.event.get() }.take()
    }

    pub async fn wait(&self) -> tau::Event<InterruptId> {
        future::poll_fn(|_cx| {
            if let Some(event) = self.take() {
                Poll::Ready(event)
            } else {
                Poll::Pending
            }
        })
        .await
    }

    pub fn complete_interrupt(&self, id: InterruptId) {
        self.plic.complete(id);
    }

    pub fn error(&self, args: fmt::Arguments<'_>) {
        self.log::<{ LogLevel::Error as u8 }>(args);
    }

    pub fn info(&self, args: fmt::Arguments<'_>) {
        self.log::<{ LogLevel::Info as u8 }>(args);
    }

    pub fn debug(&self, args: fmt::Arguments<'_>) {
        self.log::<{ LogLevel::Debug as u8 }>(args);
    }

    fn log<const LEVEL: u8>(&self, args: fmt::Arguments<'_>) {
        if LEVEL > self.log_level as u8 {
            return;
        }
        let time = tau::dbg::read_time();
        let secs = time / 4_000_000;
        let nanos = (time % 4_000_000) * 250;
        let printer = &mut self.shared_mut().uart_buffer;
        let level = match LEVEL {
            0 => "error",
            1 => "warn",
            2 => "info",
            3 => "debug",
            _ => "none",
        };
        write!(printer, "{level} {secs:03}.{nanos:09} {args}\r\n").unwrap_or_default();
    }

    pub fn shared(&self) -> &Shared {
        unsafe { &*self.shared.get() }
    }

    #[allow(clippy::mut_from_ref)]
    pub fn shared_mut(&self) -> &mut Shared {
        unsafe { &mut *self.shared.get() }
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

#[pin_project::pin_project]
struct Driver<'dtb, Fut> {
    #[pin]
    fut: Fut,
    int: &'dtb [u32],
}

impl<'dtb, Fut> Driver<'dtb, Fut> {
    pub fn parse_dtb<F>(
        config: tau::DtbProps<'dtb>,
        plic: &Plic,
        context_id: usize,
        factory: F,
    ) -> Self
    where
        F: FnOnce(tau::DtbProps<'dtb>) -> Fut,
    {
        let int = config.find_int(|name| name == "interrupts").unwrap_or(&[]);
        for num in int {
            let num = InterruptNumber::new(num.to_be());
            plic.set_priority(&num, InterruptPriority::_1);
            plic.enable(context_id, &num);
        }
        Driver {
            int,
            fut: factory(config),
        }
    }
}

#[pin_project::pin_project]
#[derive(Default)]
pub struct Drivers<'dtb, Shell, Sdio, Uart> {
    #[pin]
    shell: Shell,
    #[pin]
    uart: Option<Driver<'dtb, Uart>>,
    #[pin]
    sdio: Option<Driver<'dtb, Sdio>>,
}

pub fn drivers<'dtb>(
    dtb: tau::Dtb<'dtb>,
    rt: &Runtime,
    plic: &Plic,
    context_id: usize,
    uart_v_addr: usize,
    sdio_v_addr: usize,
) -> Drivers<'dtb, impl Future<Output = ()>, impl Future<Output = ()>, impl Future<Output = ()>> {
    let shell = shell::run(rt);
    let uart = dtb
        .iter()
        .find(|(_, path)| (path[1] == "soc" && path[2].starts_with("serial@")))
        .map(|(config, _)| {
            Driver::parse_dtb(config, plic, context_id, |config| {
                uart::run(rt, config, uart_v_addr)
            })
        });
    let sdio = dtb
        .iter()
        .find(|(_, path)| (path[1] == "soc" && path[2].starts_with("sdio1@")))
        .map(|(config, _)| {
            Driver::parse_dtb(config, plic, context_id, |config| {
                sdio::run(rt, config, sdio_v_addr)
            })
        });
    Drivers { shell, uart, sdio }
}

fn poll_match<Fut>(
    mut dri: Pin<&mut Option<Driver<'_, Fut>>>,
    num: u32,
    cx: &mut Context<'_>,
) -> Option<()>
where
    Fut: Future<Output = ()>,
{
    let mut dr = dri.as_mut().as_pin_mut()?;
    if (*dr.int)
        .contains(&num)
        .then(|| dr.as_mut().project().fut.poll(cx).is_ready())?
    {
        dri.set(None);
    }
    Some(())
}

fn poll_al<Fut>(mut dri: Pin<&mut Option<Driver<'_, Fut>>>, cx: &mut Context<'_>)
where
    Fut: Future<Output = ()>,
{
    if let Some(mut dr) = dri.as_mut().as_pin_mut() {
        if dr.as_mut().project().fut.poll(cx).is_ready() {
            dri.set(None);
        }
    }
}

impl<'dtb, Shell, Sdio, Uart> Drivers<'dtb, Shell, Sdio, Uart>
where
    Shell: Future<Output = ()>,
    Sdio: Future<Output = ()>,
    Uart: Future<Output = ()>,
{
    pub fn run(self: Pin<&mut Self>, rt: &Runtime) {
        let waker = noop_waker::noop_waker();
        let mut cx = Context::from_waker(&waker);

        let mut this = self.project();
        poll_al(this.uart.as_mut(), &mut cx);
        poll_al(this.sdio.as_mut(), &mut cx);

        while !rt.shared().terminate {
            // TODO: proper time wheel
            let deadline = tau::dbg::read_time().wrapping_add(0x10000);
            let mut event = Some(tau::Ubi::wait(NonZeroUsize::new(deadline)));

            while let Some(event) = tau::event_with(&mut event, rt.plic.next()) {
                match &event {
                    tau::Event::Signal { .. } => continue,
                    tau::Event::Interrupt(int) => {
                        let num = int.as_ref().get().to_be();
                        rt.put(event);
                        poll_match(this.sdio.as_mut(), num, &mut cx)
                            .or_else(|| poll_match(this.uart.as_mut(), num, &mut cx));
                    }
                    tau::Event::Timeout => {
                        rt.put(event);
                        poll_al(this.sdio.as_mut(), &mut cx);
                    }
                }

                if let Some(tau::Event::Interrupt(int)) = rt.take() {
                    rt.plic.complete(int);
                }
            }

            if !rt.shared().uart_buffer.is_empty() {
                rt.put(tau::Event::Signal { inv: 0, arg: 0 });
                poll_al(this.uart.as_mut(), &mut cx);
            }
            if !matches!(rt.shared().sdio_task, sdio::Task::Idle) {
                rt.put(tau::Event::Signal { inv: 0, arg: 0 });
                poll_al(this.sdio.as_mut(), &mut cx);
            }
        }
    }
}
