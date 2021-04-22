use super::{gpio::Gpio, uart::MiniUart};

pub struct State;

impl State {
    const GPIO_OFFSET: usize = 0x0020_0000;
    const UART_OFFSET: usize = 0x0021_5000;

    fn gpio(&self) -> &mut Gpio {
        unsafe { &mut *((0x3f00_0000 + Self::GPIO_OFFSET) as *mut Gpio) }
    }

    fn uart(&self) -> &mut MiniUart {
        unsafe { &mut *((0x3f00_0000 + Self::UART_OFFSET) as *mut MiniUart) }
    }

    pub fn init_uart(&mut self) {
        let gpio = self.gpio();
        let uart = self.uart();
        uart.init(gpio);
    }

    pub fn stdout(&mut self) -> &mut MiniUart {
        self.uart()
    }
}
