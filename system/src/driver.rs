use core::{hint, num::NonZeroUsize, time::Duration};

use thiserror_no_std::Error;

use super::plic::{Plic, PlicThresholdClaim, InterruptNumber, InterruptId, InterruptPriority};
use super::{uart, sdio};

pub trait DriverState
where
    Self: Sized,
{
    fn handle(&mut self, shared: &mut Shared, event: &tau::Event<InterruptId>);
}

#[derive(Default)]
pub struct Shared {
    pub uart_buffer: uart::Buffer,
    pub sdio_task: Option<sdio::Task>,
    pub sdio_done: Option<sdio::Task>,
    pub terminate: bool,
    pub deadline: Option<NonZeroUsize>,
}

impl Shared {
    pub fn sleep(&mut self, delay: Duration) {
        let d = tau::dbg::read_time() + ((delay.as_nanos() / 250) as usize);
        self.deadline = NonZeroUsize::new(d);
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

struct Driver<'dtb, Fut> {
    fut: Fut,
    int: &'dtb [u32],
}

impl<'dtb, Fut> Driver<'dtb, Fut> {
    pub fn parse_dtb<F>(
        config: tau::DtbProps<'dtb>,
        plic: &Plic,
        context_id: usize,
        factory: F,
    ) -> Option<Self>
    where
        F: FnOnce(tau::DtbProps<'dtb>) -> Option<Fut>,
    {
        let int = config.find_int(|name| name == "interrupts").unwrap_or(&[]);
        for num in int {
            let num = InterruptNumber::new(num.to_be());
            plic.set_priority(&num, InterruptPriority::_1);
            plic.enable(context_id, &num);
        }
        factory(config).map(|fut| Driver { int, fut })
    }
}

pub struct Drivers<'dtb, Uart, Sdio> {
    uart: Option<Driver<'dtb, Uart>>,
    sdio: Option<Driver<'dtb, Sdio>>,
}

pub fn drivers<'dtb>(
    dtb: tau::Dtb<'dtb>,
    plic: &Plic,
    context_id: usize,
    uart_v_addr: usize,
    sdio_v_addr: usize,
) -> Drivers<'dtb, impl DriverState, impl DriverState> {
    let uart = dtb
        .iter()
        .find(|(_, path)| (path[1] == "soc" && path[2].starts_with("serial@")))
        .and_then(|(config, _)| {
            Driver::parse_dtb(config, plic, context_id, |config| {
                Some(uart::State::new(uart::Config::parse(
                    config,
                    115200,
                    uart_v_addr,
                )?))
            })
        });
    let sdio = dtb
        .iter()
        .find(|(_, path)| (path[1] == "soc" && path[2].starts_with("sdio1@")))
        .and_then(|(config, _)| {
            Driver::parse_dtb(config, plic, context_id, |config| {
                sdio::State::new(config, sdio_v_addr)
            })
        });
    Drivers { uart, sdio }
}

impl<'dtb, Uart, Sdio> Drivers<'dtb, Uart, Sdio>
where
    Uart: DriverState,
    Sdio: DriverState,
{
    pub fn run(&mut self, plic: &'static PlicThresholdClaim) {
        let phys = 0x7000_1000;
        let page = 0x0120_0000;
        tau::Ubi::map(NonZeroUsize::new(phys as _), page, 1).unwrap_or_default();
        // let page = page as *mut [u8; 0x1000];

        let mut shared = Shared {
            deadline: NonZeroUsize::new(tau::dbg::read_time() + 0x10000),
            sdio_task: Some(sdio::Task::Read { page: 0x400, phys }),
            ..Default::default()
        };
        while !(shared.terminate && shared.uart_buffer.is_empty()) {
            // TODO: proper time wheel
            let mut event = Some(tau::Ubi::wait(shared.deadline.take()));

            while let Some(event) = tau::event_with(&mut event, plic.next()) {
                match &event {
                    tau::Event::Signal { .. } => continue,
                    tau::Event::Interrupt(int) => {
                        let num = int.as_ref().get();
                        if let Some(st) = self.uart.as_mut() {
                            if st.int.contains(&num.to_be()) {
                                st.fut.handle(&mut shared, &event)
                            }
                        }
                        if let Some(st) = self.sdio.as_mut() {
                            if st.int.contains(&num.to_be()) {
                                st.fut.handle(&mut shared, &event);
                            }
                        }
                    }
                    tau::Event::Timeout => {
                        if let Some(st) = self.sdio.as_mut() {
                            st.fut.handle(&mut shared, &event);
                        }
                    }
                }

                if let tau::Event::Interrupt(int) = event {
                    plic.complete(int);
                }
            }

            if shared.sdio_done.take().is_some() {
                let dma_data = unsafe { (0x0120_0000 as *const [u8; 0x1000]).read_volatile() };
                for chunk in dma_data.chunks(0x10) {
                    let &[a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = chunk else {
                        break;
                    };
                    shared.uart_buffer.write(format_args!(
                        "\
                        {a:02x} {b:02x} {c:02x} {d:02x} {e:02x} {f:02x} {g:02x} {h:02x} \
                        {i:02x} {j:02x} {k:02x} {l:02x} {m:02x} {n:02x} {o:02x} {p:02x}"
                    ));
                }
            }

            if !shared.uart_buffer.is_empty() {
                if let Some(st) = self.uart.as_mut() {
                    let event = tau::Event::Signal { inv: 0, arg: 0 };
                    st.fut.handle(&mut shared, &event);
                }
            }
        }
    }
}
