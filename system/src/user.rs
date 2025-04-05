use core::{
    cell::UnsafeCell,
    fmt::Write,
    future,
    pin::Pin,
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
    let phys = 0x7000_1000;
    let page = Box::<[[u8; 16]], _>::new_uninit_slice_in(0x100, tau::Area::new(phys, 0x1000));
    read(shared, phys, 0x400).await;

    let dma_data = unsafe { page.assume_init() };
    for chunk in &dma_data {
        let [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] =
            unsafe { (chunk as *const [u8; 16]).read_volatile() };
        unsafe { &mut *shared.get() }.write(format_args!(
            "\
            {a:02x} {b:02x} {c:02x} {d:02x} {e:02x} {f:02x} {g:02x} {h:02x} \
            {i:02x} {j:02x} {k:02x} {l:02x} {m:02x} {n:02x} {o:02x} {p:02x}"
        ));
    }

    loop {
        unsafe { &mut *shared.get() }
            .uart_buffer
            .write_char('C')
            .unwrap_or_default();
        sleep(shared, Duration::from_secs(1)).await;
    }
}

async fn read(shared: &UnsafeCell<Shared>, phys: usize, block: u32) {
    let task = sdio::Task::Read {
        page: block,
        phys: phys as u32,
    };
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
async fn write(shared: &UnsafeCell<Shared>, phys: usize, block: u32) {
    let task = sdio::Task::Write {
        page: block,
        phys: phys as u32,
    };
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
