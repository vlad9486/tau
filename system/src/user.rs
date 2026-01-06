use core::{
    cell::UnsafeCell,
    future,
    mem::MaybeUninit,
    pin::Pin,
    ptr,
    task::{Context, Poll},
    time::Duration,
};

use alloc::boxed::Box;

use super::{scheduler::Shared, sdio};

pub struct State<'a> {
    fut: Pin<Box<dyn Future<Output = ()> + 'a>>,
}

impl<'a> State<'a> {
    pub fn new(shared: &'a UnsafeCell<Shared>) -> Self {
        State {
            fut: Box::pin(run(shared)),
        }
    }

    pub fn step(&mut self) {
        let waker = noop_waker::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let _ = self.fut.as_mut().poll(&mut cx);
    }
}

async fn run(shared: &UnsafeCell<Shared>) {
    // TODO: allocator for DMA
    let phys = 0x7000_1000_u32;
    let base = tau::to_size(phys);
    let page = tau::Area::new(base, 0x1000).r::<MaybeUninit<[[u8; 0x10]; 0x100]>>();
    read(shared, phys, 0x600).await;

    unsafe { &mut *shared.get() }.write(format_args!("___page: 0x600"));
    let dma_data = unsafe { page.assume_init() };
    for chunk in dma_data.iter() {
        let [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] =
            unsafe { (ptr::from_ref(chunk)).read_volatile() };
        unsafe { &mut *shared.get() }.write(format_args!(
            "\
            {a:02x} {b:02x} {c:02x} {d:02x} {e:02x} {f:02x} {g:02x} {h:02x} \
            {i:02x} {j:02x} {k:02x} {l:02x} {m:02x} {n:02x} {o:02x} {p:02x}"
        ));
    }

    let mut cmd = [0; 8];
    loop {
        read_uart(shared, &mut cmd).await;
        unsafe { &mut *shared.get() }.uart_out.tx(cmd[0]);
        if cmd[0] == b'q' {
            unsafe { &mut *shared.get() }.terminate = true;
            break;
        } else if cmd[0] == b'u' {
            // receive a file by y_modem
            loop {
                unsafe { &mut *shared.get() }.uart_out.tx(y_modem::CRC);
                sleep(shared, Duration::from_secs(1)).await;
                // TODO:
            }
        }
    }
}

async fn read_uart(shared: &UnsafeCell<Shared>, b: &mut [u8]) -> usize {
    future::poll_fn(move |_| {
        let buf = &mut unsafe { &mut *shared.get() }.uart_in;
        if buf.is_empty() {
            Poll::Pending
        } else {
            Poll::Ready(buf.rxs(b))
        }
    })
    .await
}

async fn read(shared: &UnsafeCell<Shared>, phys: u32, block: u32) {
    let task = sdio::Task::Read { page: block, phys };
    unsafe { &mut *shared.get() }.sdio_task = Some(task);
    let _done = future::poll_fn(move |_| {
        if let Some(done) = unsafe { &mut *shared.get() }.sdio_done.take() {
            Poll::Ready(done)
        } else {
            Poll::Pending
        }
    })
    .await;
}

#[allow(dead_code)]
async fn write(shared: &UnsafeCell<Shared>, phys: u32, block: u32) {
    let task = sdio::Task::Write { page: block, phys };
    unsafe { &mut *shared.get() }.sdio_task = Some(task);
    let _done = future::poll_fn(move |_| {
        if let Some(done) = unsafe { &mut *shared.get() }.sdio_done.take() {
            Poll::Ready(done)
        } else {
            Poll::Pending
        }
    })
    .await;
}

async fn sleep(shared: &UnsafeCell<Shared>, duration: Duration) {
    unsafe { &mut *shared.get() }.sleep(2, duration);
    let mut sleep = false;
    future::poll_fn(|_| {
        sleep = !sleep;
        if sleep {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
}

#[allow(dead_code)]
mod y_modem {
    pub const SOH: u8 = 0x01;
    pub const STX: u8 = 0x02;
    pub const EOT: u8 = 0x04;
    pub const ACK: u8 = 0x06;
    pub const NAK: u8 = 0x15;
    pub const CAN: u8 = 0x18;
    pub const CRC: u8 = 0x43;
}
