// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{ops::Range, time::Duration};

use super::{
    Controller, DriverState, FRAMEBUFFER_PHYS, FRAMEBUFFER_RESERVATION, Hdmi, MODE, Platform,
    Register, Shared, TIMER_ISSUER, Task, UNCACHED_OFFSET,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Memory,
    Pmic,
    Power,
    Clock,
    Reset,
    Controller,
    Pll,
    Disconnected,
    Failed,
}

enum Phase {
    Start,
    Supplies,
    Power(u8),
    Connect,
    Pll(u8),
    Settle,
    Ready,
    Failed,
}

pub struct State {
    platform: Platform,
    controller: Controller,
    hdmi: Hdmi,
    framebuffer: Option<&'static [Register<u32, u32>]>,
    phase: Phase,
    reserved: bool,
    render: Option<(Task, Range<usize>)>,
    timer_armed: bool,
    initialized: bool,
}

impl State {
    pub fn new(dtb: &tau::Dtb<'_>, reservations: tau::DtbRsv<'_>) -> Option<Self> {
        // Hardcoded wiring and oscillator apply only to VisionFive 2. Reject
        // other DTs before mapping any board-specific MMIO (including QEMU).
        let board = dtb.iter().next()?.0.find_str(|n| n == "compatible")?;
        if !board
            .split('\0')
            .any(|s| s.starts_with("starfive,visionfive-v2"))
        {
            return None;
        }
        let device = |name: &str, base: u32| {
            dtb.iter().any(|(props, path)| {
                path.len() == 3
                    && path[1] == "soc"
                    && props
                        .find_str(|n| n == "compatible")
                        .is_some_and(|s| s.split('\0').any(|s| s == name))
                    && props
                        .find_str(|n| n == "status")
                        .is_none_or(|s| s == "okay")
                    && props.find_int(|n| n == "reg").is_some_and(|cells| {
                        cells.len() >= 4 && cells[0].to_be() == 0 && cells[1].to_be() == base
                    })
            })
        };
        if !device("starfive,jh7110-dc8200", 0x2940_0000)
            || !device("starfive,jh7110-hdmi", 0x2959_0000)
        {
            return None;
        }
        let reserved = reservations.into_iter().any(|(base, len)| {
            base <= FRAMEBUFFER_PHYS
                && base.saturating_add(len) >= FRAMEBUFFER_PHYS + FRAMEBUFFER_RESERVATION
        }) && dtb.iter().any(|(props, path)| {
            path.len() == 2
                && path[1].starts_with("memory@")
                && props.find_reg().is_some_and(|area| {
                    area.base <= FRAMEBUFFER_PHYS
                        && area.base.saturating_add(area.len)
                            >= FRAMEBUFFER_PHYS + FRAMEBUFFER_RESERVATION
                })
        });
        Some(Self {
            platform: Platform::new(),
            controller: Controller::new(),
            hdmi: Hdmi::new(),
            framebuffer: None,
            phase: Phase::Start,
            reserved,
            render: None,
            timer_armed: false,
            initialized: false,
        })
    }

    fn sleep(&mut self, shared: &mut Shared, millis: u64) {
        if !self.timer_armed {
            shared.sleep(TIMER_ISSUER, Duration::from_millis(millis));
            self.timer_armed = true;
        }
    }

    fn fail(&mut self, shared: &mut Shared, error: Error) {
        // VOUT registers must not be accessed before power/clocks are ready.
        if self.initialized {
            self.controller.stop();
            self.hdmi.disable();
        }
        self.phase = Phase::Failed;
        self.render = None;
        shared.write(format_args!("display: failed: {error:?}"));
        if shared.display_done.is_none() && shared.display_task.take().is_some() {
            shared.display_done = Some(Err(error));
        }
    }

    fn advance(&mut self, shared: &mut Shared) -> Result<(), Error> {
        match self.phase {
            Phase::Start => {
                if !self.reserved {
                    return Err(Error::Memory);
                }
                self.platform.supplies()?;
                self.phase = Phase::Supplies;
                self.sleep(shared, 1);
            }
            Phase::Supplies => {
                self.platform.power_on()?;
                self.phase = Phase::Power(0);
                self.sleep(shared, 1);
            }
            Phase::Power(attempt) => {
                if !self.platform.powered() {
                    if attempt >= 100 {
                        return Err(Error::Power);
                    }
                    self.phase = Phase::Power(attempt + 1);
                    self.sleep(shared, 1);
                    return Ok(());
                }
                self.platform.configure()?;
                self.initialized = true;
                self.controller.stop();
                let (revision, cid) = self.controller.identity();
                if revision != 0x5720 && revision != 0x5721 {
                    return Err(Error::Controller);
                }
                shared.write(format_args!(
                    "display: DC8200 revision={revision:x} cid={cid:x}"
                ));
                self.framebuffer =
                    Some(tau::Area::new(FRAMEBUFFER_PHYS + UNCACHED_OFFSET, MODE.bytes()).sl());
                // Initialize every visible pixel before allowing DMA scanout.
                self.render = Some((Task::TestPattern, 0..MODE.height as usize));
                self.phase = Phase::Connect;
                shared.write(format_args!(
                    "display: waiting for HDMI, fixed 1920x1080@60"
                ));
                self.sleep(shared, 1);
            }
            Phase::Connect => {
                if self.render.is_some() {
                    self.paint();
                    self.sleep(shared, 1);
                } else if self.hdmi.connected() {
                    self.hdmi.prepare();
                    self.phase = Phase::Pll(0);
                    self.sleep(shared, 1);
                } else {
                    self.sleep(shared, 250);
                }
            }
            Phase::Pll(attempt) => {
                if !self.hdmi.connected() {
                    self.hdmi.disable();
                    self.phase = Phase::Connect;
                    self.sleep(shared, 250);
                } else if self.hdmi.locked() {
                    self.controller.start();
                    self.hdmi.enable();
                    self.phase = Phase::Settle;
                    self.sleep(shared, 50);
                } else {
                    if attempt >= 100 {
                        return Err(Error::Pll);
                    }
                    self.phase = Phase::Pll(attempt + 1);
                    self.sleep(shared, 1);
                }
            }
            Phase::Settle => {
                if !self.hdmi.connected() {
                    self.controller.stop();
                    self.hdmi.disable();
                    self.phase = Phase::Connect;
                    self.sleep(shared, 250);
                    return Ok(());
                }
                self.hdmi.sync();
                self.phase = Phase::Ready;
                shared.write(format_args!("display: ready, ARGB8888 stride={} phys={FRAMEBUFFER_PHYS:x}; f=bars c=clear r=rectangle", MODE.stride()));
                self.submit(shared);
                self.sleep(shared, 20);
            }
            Phase::Ready => {
                if !self.hdmi.connected() {
                    self.controller.stop();
                    self.hdmi.disable();
                    if self.render.take().is_some() {
                        shared.display_done = Some(Err(Error::Disconnected));
                    }
                    self.phase = Phase::Connect;
                    shared.write(format_args!("display: HDMI disconnected"));
                    self.sleep(shared, 250);
                } else {
                    if self.render.is_some() && self.paint() {
                        shared.display_done = Some(Ok(()));
                    }
                    self.submit(shared);
                    self.sleep(shared, if self.render.is_some() { 1 } else { 20 });
                }
            }
            Phase::Failed => self.submit(shared),
        }
        Ok(())
    }

    fn paint(&mut self) -> bool {
        let Some((task, rows)) = self.render.as_mut() else {
            return false;
        };
        let framebuffer = self.framebuffer.expect("mapped before rendering");
        let end = (rows.start + 32).min(rows.end);
        for y in rows.start..end {
            task.row(MODE.width as usize, MODE.height as usize, y, |x, color| {
                framebuffer[y * (MODE.stride() / 4) + x].write(color);
            });
        }
        // Include device accesses as well as memory: the uncached RAM alias
        // and the controller's MMIO must observe all completed pixel writes.
        unsafe { core::arch::asm!("fence iorw, iorw") };
        rows.start = end;
        if rows.start >= rows.end {
            self.render = None;
            true
        } else {
            false
        }
    }

    pub fn submit(&mut self, shared: &mut Shared) {
        if shared.display_done.is_some() || self.render.is_some() {
            return;
        }
        if matches!(self.phase, Phase::Failed) {
            if shared.display_task.take().is_some() {
                shared.display_done = Some(Err(Error::Failed));
            }
        } else if matches!(self.phase, Phase::Ready)
            && let Some(task) = shared.display_task.take()
        {
            self.render = Some((task, task.rows(MODE.height as usize)));
            if self.paint() {
                shared.display_done = Some(Ok(()));
            } else {
                self.sleep(shared, 1);
            }
        }
    }
}

impl DriverState for State {
    fn handle(&mut self, shared: &mut Shared, event: tau::Event) {
        if event != tau::Event::Timeout {
            return;
        }
        self.timer_armed = false;
        if let Err(error) = self.advance(shared) {
            self.fail(shared, error);
        }
    }
}
