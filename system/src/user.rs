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

use super::{ethernet, register::Register, scheduler::Shared, sdio};

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
    {
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
    }

    // TODO: DMA allocator. Descriptor pages are 0x70002000 / 0x70004000.
    let tx_phys = [0x7000_3000u32, 0x7000_5000];
    let tx_buffers =
        tx_phys.map(|phys| tau::Area::new(phys as usize, 0x1000).r::<[Register<u8, u8>; 0x1000]>());
    let mut tx_usable = [true, false];
    let mut sequence = 0u32;
    let mut cmd = [0; 1];
    loop {
        read_uart(shared, &mut cmd).await;
        unsafe { &mut *shared.get() }.uart_out.tx(cmd[0]);
        match cmd[0] {
            b'q' => {
                unsafe { &mut *shared.get() }.terminate = true;
                break;
            }
            b't' => {
                for port in 0..2 {
                    if !tx_usable[port] {
                        continue;
                    }
                    let packet = frame::build(port as u8, sequence);
                    for (dst, byte) in tx_buffers[port].iter().zip(packet) {
                        dst.write(byte);
                    }
                    tau::asm::fence();
                    let result = transmit(shared, port, tx_phys[port], frame::LEN as u16).await;
                    if result == Err(ethernet::TxError::BufferUnavailable) {
                        tx_usable[port] = false;
                    }
                    unsafe { &mut *shared.get() }.write(format_args!(
                        "eth{port}: user TX seq={sequence}: {result:?}"
                    ));
                }
                sequence = sequence.wrapping_add(1);
            }
            _ => {}
        }
    }
}

async fn transmit(
    shared: &UnsafeCell<Shared>,
    port: usize,
    phys: u32,
    len: u16,
) -> ethernet::TxDone {
    // This coroutine is the sole producer; it always consumes the completion
    // before submitting again or touching the buffer.
    unsafe { &mut *shared.get() }.ethernet_task[port] = Some(ethernet::TxTask { phys, len });
    future::poll_fn(
        |_| match unsafe { &mut *shared.get() }.ethernet_done[port].take() {
            Some(done) => Poll::Ready(done),
            None => Poll::Pending,
        },
    )
    .await
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

#[allow(dead_code)]
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

mod frame {
    //! A raw experimental Ethernet frame; no IP/ARP or network stack.
    pub const LEN: usize = 60; // MAC appends four FCS bytes.

    pub fn build(port: u8, sequence: u32) -> [u8; LEN] {
        let mut frame = [0u8; LEN];
        frame[..6].fill(0xff);
        frame[6..12].copy_from_slice(&[0x02, 0x54, 0x41, 0x55, 0, port + 1]);
        frame[12..14].copy_from_slice(&[0x88, 0xb5]);
        frame[14..22].copy_from_slice(b"TAU-TEST");
        frame[22] = port;
        frame[23] = 1;
        frame[24..28].copy_from_slice(&sequence.to_be_bytes());
        frame[28..].copy_from_slice(b"Hello from Tau on VisionFive 2! ");
        frame
    }

    // Can run on the host independently of Tau's RISC-V runtime:
    // rustc --edition=2024 --test system/src/user/frame.rs -o /tmp/tau-frame-test
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn capture_format_and_minimum_size() {
            let frame = build(0, 0x12345678);
            assert_eq!(frame.len(), 60);
            assert_eq!(
                &frame[..14],
                &[
                    255, 255, 255, 255, 255, 255, 2, 0x54, 0x41, 0x55, 0, 1, 0x88, 0xb5
                ]
            );
            assert_eq!(&frame[14..24], b"TAU-TEST\x00\x01");
            assert_eq!(&frame[24..28], &[0x12, 0x34, 0x56, 0x78]);
            assert_eq!(&frame[28..], b"Hello from Tau on VisionFive 2! ");
        }

        #[test]
        fn ports_have_distinct_unicast_local_addresses() {
            let a = build(0, 0);
            let b = build(1, u32::MAX);
            assert_ne!(&a[6..12], &b[6..12]);
            assert_eq!(b[6] & 3, 2);
            assert_eq!(b[11], 2);
            assert_eq!(b[22], 1);
            assert_eq!(&b[24..28], &[255; 4]);
        }
    }
}
