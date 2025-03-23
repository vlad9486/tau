use core::{
    cell::UnsafeCell,
    fmt::{self, Write as _},
    future, hint,
    task::Poll,
};

use thiserror_no_std::Error;

use super::{
    plic::{PlicThresholdClaim, InterruptId},
    uart::{UartIo, UartPrinter},
};

#[derive(Clone, Copy)]
pub struct Runtime<'a> {
    pub plic_claim: &'a PlicThresholdClaim,
    pub uart: &'a dyn UartIo,
    pub interrupt: &'a UnsafeCell<Option<InterruptId>>,
}

impl Runtime<'_> {
    pub async fn wait_interrupt(&self) -> InterruptId {
        future::poll_fn(|_cx| {
            if let Some(id) = unsafe { &mut *self.interrupt.get() }.take() {
                Poll::Ready(id)
            } else {
                Poll::Pending
            }
        })
        .await
    }

    pub fn complete_interrupt(&self, id: InterruptId) {
        self.plic_claim.complete(id);
    }

    pub fn error(&self, args: fmt::Arguments<'_>) {
        let time = tau::dbg::read_time();
        write!(UartPrinter(self.uart), "ERROR {time:010} {args}\r\n").unwrap_or_default();
    }

    pub fn info(&self, args: fmt::Arguments<'_>) {
        let time = tau::dbg::read_time();
        write!(UartPrinter(self.uart), "INFO {time:010} {args}\r\n").unwrap_or_default();
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
