use core::{ffi, fmt, hint, num::NonZeroUsize};

use super::{register::Register, driver::Runtime};

pub async fn run(rt: &Runtime, config: tau::DtbProps<'_>, v_addr: usize) {
    let Some([addr_hi, addr_lo, size_hi, size_lo]) = config.find_int(|name| name == "reg") else {
        return;
    };
    let addr = ((addr_hi.to_be() as usize) << 32) + (addr_lo.to_be() as usize);
    let size = ((size_hi.to_be() as usize) << 32) + (size_lo.to_be() as usize);
    tau::Ubi::map(NonZeroUsize::new(addr), v_addr, size.div_ceil(0x1000)).unwrap_or_default();

    let reg_width = config
        .find_int(|name| name == "reg-io-width")
        .and_then(|x| x.first().copied().map(u32::to_be))
        .unwrap_or(1);

    let uart = if reg_width == 1 {
        unsafe { &*(v_addr as *mut Uart<true, u8>) }.init(115200) as &dyn UartIo
    } else if reg_width == 4 {
        unsafe { &*(v_addr as *mut Uart<false, u32>) }.init(115200) as &dyn UartIo
    } else {
        return;
    };

    'main: loop {
        let event = rt.wait().await;

        if let tau::Event::Interrupt(id) = event {
            rt.complete_interrupt(id);
        }

        while let Some(c) = uart.rx() {
            uart.tx(c);
            if c == b'\r' {
                uart.tx(b'\n');
                break 'main;
            }
        }

        let buf = &mut rt.shared_mut().uart_buffer;
        while !buf.is_empty() {
            let b = buf.buf[buf.cons % Buffer::SIZE];
            if uart.tx(b) {
                buf.cons += 1;
            } else {
                break;
            }
        }
        uart.tx_int(!buf.is_empty());
    }

    rt.shared_mut().terminate = true;
}

#[repr(C, align(0x1000))]
struct Uart<const UART_16550_COMPATIBLE: bool, Word> {
    transmit_holding: Register<Word, Word>,
    interrupt_enable: Register<Word, Word>,
    interrupt_status_fifo_control: Register<Word, Word>,
    line_control: Register<Word, Word>,
    modem_control: Register<Word, Word>,
    line_status: Register<Word, ffi::c_void>,
    modem_status: Register<Word, Word>,
    scratchpad: Register<Word, Word>,
    _hole: [Word; 23],
    uart_status: Register<Word, ffi::c_void>,
}

impl<const UART_16550_COMPATIBLE: bool, Word> Uart<UART_16550_COMPATIBLE, Word>
where
    Word: From<u8> + Into<u32>,
{
    const UART0_CLOCK_FREQ: u32 = 24_000_000;

    #[inline(always)]
    fn init(&self, baud_rate: u32) -> &Self {
        self.set_line_control(0b10000011);
        {
            let divisor = Self::UART0_CLOCK_FREQ / (16 * baud_rate);
            self.transmit_holding.write((divisor & 0xff) as u8);
            self.interrupt_enable.write(((divisor >> 8) & 0xff) as u8);
        }
        self.interrupt_status_fifo_control.write(0b00000110);
        self.interrupt_status_fifo_control.write(0b00000001);

        self.set_line_control(0b00000011);
        self.interrupt_enable.write(0b00000001);
        self
    }

    #[inline(always)]
    fn set_line_control(&self, v: u8) {
        if !UART_16550_COMPATIBLE {
            while self.uart_status.read().into() & 0b00000001 != 0 {
                hint::spin_loop();
            }
        }
        self.line_control.write(v);
    }
}

trait UartIo {
    fn rx(&self) -> Option<u8>;

    fn tx(&self, b: u8) -> bool;

    fn tx_int(&self, on: bool);
}

impl<const UART_STATUS: bool, Word> UartIo for Uart<UART_STATUS, Word>
where
    Word: From<u8> + Into<u32>,
{
    #[inline(always)]
    fn rx(&self) -> Option<u8> {
        if self.line_status.read().into() & 0b0000_0001 != 0 {
            Some(self.transmit_holding.read().into() as _)
        } else {
            None
        }
    }

    #[inline(always)]
    fn tx(&self, b: u8) -> bool {
        if self.line_status.read().into() & 0b0010_0000 != 0 {
            self.transmit_holding.write(b);
            true
        } else {
            false
        }
    }

    #[inline(always)]
    fn tx_int(&self, on: bool) {
        self.interrupt_enable
            .write(0b00000001 | (u8::from(on) << 1));
    }
}

pub struct Buffer {
    buf: [u8; Self::SIZE],
    pos: usize,
    cons: usize,
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer {
            buf: [0; Self::SIZE],
            pos: 0,
            cons: 0,
        }
    }
}

impl Buffer {
    const SIZE: usize = 0x4000;

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn len(&self) -> usize {
        self.pos.saturating_sub(self.cons)
    }
}

impl fmt::Write for Buffer {
    fn write_char(&mut self, c: char) -> fmt::Result {
        let i = self.pos;
        self.buf[i % Self::SIZE] = c as _;
        self.pos = i + 1;
        Ok(())
    }

    fn write_str(&mut self, s: &str) -> fmt::Result {
        let pos = self.pos;
        let end = pos + s.len();
        if end / Self::SIZE == pos / Self::SIZE {
            self.buf[(pos % Self::SIZE)..(end % Self::SIZE)].clone_from_slice(s.as_bytes());
        } else {
            let mid = Self::SIZE - (pos % Self::SIZE);
            self.buf[(pos % Self::SIZE)..].clone_from_slice(&s.as_bytes()[..mid]);
            self.buf[..(end % Self::SIZE)].clone_from_slice(&s.as_bytes()[mid..]);
        }
        self.pos = end;
        Ok(())
    }
}
