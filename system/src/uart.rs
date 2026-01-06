use core::{
    ffi,
    fmt::{self, Write as _},
    hint,
    num::NonZeroU32,
};

use super::{
    register::Register,
    scheduler::{DriverState, Shared},
};

pub struct Config {
    pub area: tau::Area,
    pub reg_io_width: u32,
    pub baud_rate: u32,
}

impl Config {
    pub fn parse(props: tau::DtbProps<'_>, baud_rate: u32) -> Option<Self> {
        let area = props.find_reg()?;

        let reg_io_width = props
            .find_int(|name| name == "reg-io-width")
            .map_or(Some(1), |x| x.first().copied().map(u32::to_be))?;
        if ![1, 4].contains(&reg_io_width) {
            return None;
        }

        Some(Config {
            area,
            reg_io_width,
            baud_rate,
        })
    }
}

pub struct State {
    reg: &'static dyn UartIo,
    baud_rate: Option<NonZeroU32>,
}

impl State {
    pub fn new(config: Config) -> Self {
        let Config {
            area,
            reg_io_width,
            baud_rate,
        } = config;

        let reg = if reg_io_width == 1 {
            area.r::<Uart<true, u8>>() as &'static dyn UartIo
        } else if reg_io_width == 4 {
            area.r::<Uart<false, u32>>() as &'static dyn UartIo
        } else {
            unreachable!()
        };
        let baud_rate = NonZeroU32::new(baud_rate);

        State { reg, baud_rate }
    }
}

impl DriverState for State {
    fn handle(&mut self, shared: &mut Shared, _event: tau::Event) {
        let uart = &self.reg;
        if let Some(baud_rate) = self.baud_rate.take() {
            return uart.init(baud_rate.get());
        }

        match uart.int_status() & 0b1111 {
            // modem status
            0b0000 => {
                panic!();
            }
            // no interrupt pending
            0b0001 => {}
            // THR empty
            0b0010 => {
                let buf = &mut shared.uart_out;
                let mut rem = 16;
                while !buf.is_empty() {
                    let b = buf.buf[buf.cons % Buffer::SIZE];
                    uart.tx(b);
                    rem -= 1;
                    buf.cons += 1;
                    if rem == 0 {
                        break;
                    }
                }
            }
            // received data available
            0b0100 => {
                while let Some(c) = uart.rx() {
                    shared.uart_in.tx(c);
                }
            }
            // receiver line status
            0b0110 => {
                panic!();
            }
            // busy detect
            0b0111 => {
                panic!();
            }
            // character timeout
            0b1100 => {
                panic!();
            }
            _ => unreachable!(),
        }

        uart.tx_int(!shared.uart_out.is_empty());
    }
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
    fn init(&self, baud_rate: u32);

    fn rx(&self) -> Option<u8>;

    fn tx(&self, b: u8);

    fn tx_int(&self, on: bool);

    fn int_status(&self) -> u8;
}

impl<const UART_16550_COMPATIBLE: bool, Word> UartIo for Uart<UART_16550_COMPATIBLE, Word>
where
    Word: From<u8> + Into<u32>,
{
    #[inline(always)]
    fn init(&self, baud_rate: u32) {
        self.set_line_control(0b10000011);
        {
            let divisor = Self::UART0_CLOCK_FREQ / (16 * baud_rate);
            self.transmit_holding.write((divisor & 0xff) as u8);
            self.interrupt_enable.write(((divisor >> 8) & 0xff) as u8);
        }
        self.interrupt_status_fifo_control.write(0b0000111);

        self.set_line_control(0b00000011);
        self.interrupt_enable.write(0b10000001);
    }

    #[inline(always)]
    fn rx(&self) -> Option<u8> {
        if self.line_status.read().into() & 0b0000_0001 != 0 {
            Some(self.transmit_holding.read().into() as _)
        } else {
            None
        }
    }

    #[inline(always)]
    fn tx(&self, b: u8) {
        self.transmit_holding.write(b);
    }

    #[inline(always)]
    fn tx_int(&self, on: bool) {
        self.interrupt_enable
            .write(0b00000001 | (u8::from(on) << 1));
    }

    #[inline(always)]
    fn int_status(&self) -> u8 {
        self.interrupt_status_fifo_control.read().into() as _
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
    const SIZE: usize = 0x8000;

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn len(&self) -> usize {
        self.pos.saturating_sub(self.cons)
    }

    pub fn write(&mut self, nanos: u128, args: fmt::Arguments<'_>) {
        let secs = nanos / 1_000_000_000;
        let subsec_nanos = nanos % 1_000_000_000;
        write!(self, "{secs:03}.{subsec_nanos:09} {args}\r\n").unwrap_or_default();
    }

    pub fn tx(&mut self, b: u8) {
        let i = self.pos;
        self.buf[i % Self::SIZE] = b;
        self.pos = i + 1;
    }

    pub fn rxs(&mut self, b: &mut [u8]) -> usize {
        let cons = self.cons;
        let end = (cons + b.len()).min(self.pos);
        let read = end - cons;

        if end / Self::SIZE == cons / Self::SIZE {
            b[..read].clone_from_slice(&self.buf[(cons % Self::SIZE)..(end % Self::SIZE)]);
        } else {
            let mid = Self::SIZE - (cons % Self::SIZE);
            b[..mid].clone_from_slice(&self.buf[(cons % Self::SIZE)..]);
            b[mid..read].clone_from_slice(&self.buf[..(end % Self::SIZE)]);
        }
        self.cons = end;
        read
    }

    pub fn txs(&mut self, b: &[u8]) {
        let pos = self.pos;
        let end = pos + b.len();
        if end / Self::SIZE == pos / Self::SIZE {
            self.buf[(pos % Self::SIZE)..(end % Self::SIZE)].clone_from_slice(b);
        } else {
            let mid = Self::SIZE - (pos % Self::SIZE);
            self.buf[(pos % Self::SIZE)..].clone_from_slice(&b[..mid]);
            self.buf[..(end % Self::SIZE)].clone_from_slice(&b[mid..]);
        }
        self.pos = end;
    }
}

impl fmt::Write for Buffer {
    fn write_char(&mut self, c: char) -> fmt::Result {
        self.tx(c as _);
        Ok(())
    }

    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.txs(s.as_bytes());
        Ok(())
    }
}
