use core::{
    ffi,
    fmt::{self, Write as _},
    hint,
};

use alloc::boxed::Box;

use super::{
    register::Register,
    plic::InterruptId,
    driver::{DriverState, Shared},
};

pub struct Config {
    pub addr: usize,
    pub size: usize,
    pub reg_io_width: u32,
    pub baud_rate: u32,
}

impl Config {
    pub fn parse(props: tau::DtbProps<'_>, baud_rate: u32) -> Option<Self> {
        let Some([addr_hi, addr_lo, size_hi, size_lo]) = props.find_int(|name| name == "reg")
        else {
            return None;
        };
        let addr = ((addr_hi.to_be() as usize) << 32) + (addr_lo.to_be() as usize);
        let size = ((size_hi.to_be() as usize) << 32) + (size_lo.to_be() as usize);
        let reg_io_width = props
            .find_int(|name| name == "reg-io-width")
            .and_then(|x| x.first().copied().map(u32::to_be))?;
        if ![1, 4].contains(&reg_io_width) {
            return None;
        }

        Some(Config {
            addr,
            size,
            reg_io_width,
            baud_rate,
        })
    }
}

pub struct State {
    reg: Box<dyn UartIo, tau::Area>,
}

impl State {
    pub fn new(config: Config) -> Self {
        let Config {
            addr,
            size,
            reg_io_width,
            baud_rate,
        } = config;

        let a = tau::Area::new(addr, size);
        let reg = if reg_io_width == 1 {
            let io = unsafe { Box::<Uart<true, u8>, _>::new_uninit_in(a).assume_init() };
            io.init(baud_rate);
            io as Box<dyn UartIo, tau::Area>
        } else if reg_io_width == 4 {
            let io = unsafe { Box::<Uart<false, u32>, _>::new_uninit_in(a).assume_init() };
            io.init(baud_rate);
            io as Box<dyn UartIo, tau::Area>
        } else {
            unreachable!()
        };

        State { reg }
    }
}

impl DriverState for State {
    fn handle(&mut self, shared: &mut Shared, _event: &tau::Event<InterruptId>) {
        let uart = &self.reg;
        let buf = &mut shared.uart_buffer;

        match uart.int_status() & 0b1111 {
            // modem status
            0b0000 => {
                panic!();
            }
            // no interrupt pending
            0b0001 => {}
            // THR empty
            0b0010 => {
                while !buf.is_empty() {
                    let b = buf.buf[buf.cons % Buffer::SIZE];
                    if uart.tx(b) {
                        buf.cons += 1;
                    } else {
                        break;
                    }
                }
            }
            // received data available
            0b0100 => {
                while let Some(c) = uart.rx() {
                    buf.write_fmt(format_args!("{c:02x} ")).unwrap_or_default();
                    if c == b'\r' {
                        buf.write_str("\r\n").unwrap_or_default();
                        shared.terminate = true;
                    }
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

        uart.tx_int(!buf.is_empty());
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
    fn init(&self, baud_rate: u32) {
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

    fn int_status(&self) -> u8;
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
    const SIZE: usize = 0x4000;

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
