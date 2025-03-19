//! Work in progress

use core::{arch, fmt};

use super::{
    plic::{PlicThresholdClaim, InterruptNumber},
    register::Register,
};

#[repr(C, align(0x1000))]
pub struct Device {
    ctrl: Register<u32, u32>,
    _pwren: Register<u32, u32>,
    _clkdiv: Register<u32, u32>,
    _clksrc: Register<u32, u32>,
    _clkena: Register<u32, u32>,
    _tmout: Register<u32, u32>,
    _ctype: Register<u32, u32>,
    _blksiz: Register<u32, u32>,
    _bytcnt: Register<u32, u32>,
    int_mask: Register<u32, u32>,
    cmd_arg: Register<u32, u32>,
    cmd: Register<u32, u32>,
    resp0: Register<u32, u32>,
    resp1: Register<u32, u32>,
    resp2: Register<u32, u32>,
    resp3: Register<u32, u32>,
    mask_int_status: Register<u32, u32>,
    recv_int_status: Register<u32, u32>,
    status: Register<u32, u32>,
}

impl Device {
    pub fn init(&self, writer: &mut impl fmt::Write, plic_tc: &PlicThresholdClaim) {
        self.recv_int_status.write(u32::MAX);
        self.int_mask.write(0u32);
        self.recv_int_status.write(u32::MAX);
        self.int_mask.write(1u32 << 2);
        self.ctrl.write(1u32 << 4);

        //  | 1 << 15 | 1 << 29
        self.send_cmd(writer, 0, 0, 0);
        self.send_cmd(writer, 8, 0x000001aa, 1 << 6 | 1 << 8 | 1 << 29);

        tau::Ubi::wait();
        if let Some(int) = plic_tc.next() {
            if int == InterruptNumber::new(75) {
                writeln!(writer, "got interrupt!\r").unwrap_or_default();
            }
            plic_tc.complete(int);
        }

        let _r = self.resp0.read();
    }

    pub fn send_cmd(&self, writer: &mut impl fmt::Write, cmd_index: u32, cmd_arg: u32, flags: u32) {
        self.cmd_arg.write(cmd_arg);
        unsafe { arch::asm!("fence ow, ow") };
        self.cmd.write(1 << 31 | cmd_index | flags);
        writeln!(writer, "sent: {cmd_index:032b} {cmd_arg:08x}\r").unwrap_or_default();
    }
}
