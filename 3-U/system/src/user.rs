// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

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

use super::{display, ethernet, register::Register, scheduler::Shared, sdio};

pub struct State<'a> {
    fut: Pin<Box<dyn Future<Output = ()> + 'a>>,
}

impl<'a> State<'a> {
    pub fn new(
        shared: &'a UnsafeCell<Shared>,
        has_sdio: bool,
        has_ethernet: bool,
        has_display: bool,
        sdio_slot: u16,
    ) -> Self {
        State {
            fut: Box::pin(run(shared, has_sdio, has_ethernet, has_display, sdio_slot)),
        }
    }

    pub fn step(&mut self) {
        let waker = noop_waker::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let _ = self.fut.as_mut().poll(&mut cx);
    }
}

fn measure_roundtrips(shared: &UnsafeCell<Shared>, slot: u16) {
    let mut samples = [0; 256];
    for (index, cycles) in samples.iter_mut().enumerate() {
        let value = index + 1;
        let start = tau::asm::read_cycle();
        let response = tau::Ubi::invoke::<1, 2>(slot, 0, [value]);
        *cycles = tau::asm::read_cycle().wrapping_sub(start);
        let [reply, count] = response.expect("invoke dummy sdio");
        assert_eq!(reply, value.rotate_left(7) ^ 0x5344_494f);
        assert_eq!(count, value);
    }

    let mean = samples[2..].iter().map(|x| *x as f32).sum::<f32>() / 254.0;
    samples.sort();
    let median = samples[126];
    unsafe { &mut *shared.get() }.write(format_args!(
        "sdio roundtrip mean: {mean}, median: {median}"
    ));
}

async fn run(
    shared: &UnsafeCell<Shared>,
    has_sdio: bool,
    has_ethernet: bool,
    has_display: bool,
    sdio_slot: u16,
) {
    measure_roundtrips(shared, sdio_slot);
    if has_display {
        unsafe { &mut *shared.get() }.display_task = Some(display::Task::TestPattern);
    }
    if has_sdio {
        // TODO: allocator for DMA
        let phys = 0x7000_1000_u32;
        let base = tau::to_size(phys);
        let page = tau::Area::new(base, 0x1000).r::<MaybeUninit<[[u8; 0x10]; 0x100]>>();

        read_sdio(shared, phys, 0x600).await;

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
    let mut tx_pending = [None; 2];

    // User-owned RX pages, separate from both descriptor rings and TX buffers.
    let rx_phys = [0x7000_6000u32, 0x7000_7000];
    let rx_buffers =
        rx_phys.map(|phys| tau::Area::new(phys as usize, 0x1000).r::<[Register<u8, u8>; 0x1000]>());
    if has_ethernet {
        for (port, phys) in rx_phys.into_iter().enumerate() {
            post_receive(shared, port, phys);
        }
    }
    let mut sequence = 0u32;
    let mut display_pending = has_display;
    let mut cmd = [0; 1];
    loop {
        match wait_event(shared).await {
            Event::Display => {
                if let Some(done) = unsafe { &mut *shared.get() }.display_done.take() {
                    display_pending = false;
                    unsafe { &mut *shared.get() }
                        .write(format_args!("display: user draw: {done:?}"));
                }
            }
            Event::Ethernet => {
                for port in 0..2 {
                    let done = unsafe { &mut *shared.get() }.ethernet_tx_done[port].take();
                    if let Some(result) = done {
                        let seq = tx_pending[port].take();
                        if result == Err(ethernet::TxError::BufferUnavailable) {
                            tx_usable[port] = false;
                        }
                        unsafe { &mut *shared.get() }
                            .write(format_args!("eth{port}: user TX seq={seq:?}: {result:?}"));
                    }
                    let done = unsafe { &mut *shared.get() }.ethernet_rx_done[port].take();
                    if let Some(result) = done {
                        match result {
                            Ok(len) => {
                                tau::asm::fence();
                                let header: [u8; 14] =
                                    core::array::from_fn(|i| rx_buffers[port][i].read());
                                let ether_type = u16::from_be_bytes([header[12], header[13]]);
                                unsafe { &mut *shared.get() }.write(format_args!(
                                    "eth{port}: RX len={len} dst={:02x?} src={:02x?} type={ether_type:04x}",
                                    &header[..6], &header[6..12]
                                ));
                            }
                            Err(error) => unsafe { &mut *shared.get() }
                                .write(format_args!("eth{port}: RX {error:?}")),
                        }
                        // Malformed frames return ownership; fatal failures
                        // disable RX, and may require quarantining the buffer.
                        if matches!(
                            result,
                            Ok(_)
                                | Err(ethernet::RxError::Descriptor
                                    | ethernet::RxError::Fragmented
                                    | ethernet::RxError::InvalidLength)
                        ) {
                            post_receive(shared, port, rx_phys[port]);
                        }
                    }
                }
            }
            Event::Uart => {
                let buf = &mut unsafe { &mut *shared.get() }.uart_in;
                buf.rxs(&mut cmd);
                match cmd[0] {
                    b'f' | b'c' | b'r' if has_display => {
                        let task = match cmd[0] {
                            b'f' => display::Task::TestPattern,
                            b'c' => display::Task::Clear { color: 0x102030 },
                            _ => display::Task::FillRect {
                                x: 320,
                                y: 240,
                                width: 640,
                                height: 480,
                                color: 0xff8000,
                            },
                        };
                        let shared = unsafe { &mut *shared.get() };
                        // Exactly one user request can be queued or in flight.
                        // The completion is consumed by Event::Display.
                        if !display_pending {
                            shared.display_task = Some(task);
                            display_pending = true;
                        } else {
                            shared.write(format_args!("display: draw busy"));
                        }
                    }
                    b'q' => {
                        unsafe { &mut *shared.get() }.terminate = true;
                        break;
                    }
                    b't' => {
                        for port in 0..2 {
                            if !tx_usable[port] || tx_pending[port].is_some() {
                                continue;
                            }
                            let packet = frame::build(port as u8, sequence);
                            for (dst, byte) in tx_buffers[port].iter().zip(packet) {
                                dst.write(byte);
                            }
                            tau::asm::fence();
                            unsafe { &mut *shared.get() }.ethernet_tx_task[port] =
                                Some(ethernet::TxTask {
                                    phys: tx_phys[port],
                                    len: frame::LEN as u16,
                                });
                            tx_pending[port] = Some(sequence);
                        }
                        sequence = sequence.wrapping_add(1);
                    }
                    _ => {
                        unsafe { &mut *shared.get() }.uart_out.tx(cmd[0]);
                    }
                }
            }
        }
    }
}

fn post_receive(shared: &UnsafeCell<Shared>, port: usize, phys: u32) {
    tau::asm::fence();
    unsafe { &mut *shared.get() }.ethernet_rx_task[port] = Some(ethernet::RxTask {
        phys,
        capacity: 2048,
    });
}

enum Event {
    Uart,
    Ethernet,
    Display,
}

async fn wait_event(shared: &UnsafeCell<Shared>) -> Event {
    future::poll_fn(move |_| {
        let shared = unsafe { &mut *shared.get() };
        if !shared.uart_in.is_empty() {
            return Poll::Ready(Event::Uart);
        }
        if shared.display_done.is_some() {
            return Poll::Ready(Event::Display);
        }
        if shared.ethernet_rx_done.iter().any(Option::is_some)
            || shared.ethernet_tx_done.iter().any(Option::is_some)
        {
            return Poll::Ready(Event::Ethernet);
        }
        Poll::Pending
    })
    .await
}

async fn read_sdio(shared: &UnsafeCell<Shared>, phys: u32, block: u32) {
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
    // rustc --edition=2024 --test 3-U/system/src/user/frame.rs -o /tmp/tau-frame-test
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
