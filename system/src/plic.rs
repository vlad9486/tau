use core::num::NonZeroU32;

use super::register::Register;

#[repr(transparent)]
pub struct InterruptNumber(NonZeroU32);

#[repr(C, align(0x1000))]
pub struct Plic {
    priorities: [Register<u32, u32>; 0x400],
    pending_bit: [Register<u32, u32>; 0x20],
    _0: [u32; 0x3e0],
    enable_bit: [[Register<u32, u32>; 0x20]; 0x3e00],
}

#[repr(C)]
pub struct PlicThresholdClaim {
    threshold: Register<InterruptPriority, InterruptPriority>,
    claim: Register<Option<InterruptId>, InterruptId>,
    _0: [u32; 0x3fe],
}

#[repr(transparent)]
pub struct InterruptId(NonZeroU32);

impl InterruptNumber {
    #[inline(always)]
    pub const fn new(value: u32) -> Self {
        Self(unsafe { NonZeroU32::new_unchecked(value) })
    }

    #[inline(always)]
    pub const fn as_int(&self) -> u32 {
        self.0.get()
    }

    #[inline(always)]
    pub fn hi(&self) -> usize {
        ((self.0.get() >> 5) & 0x1f) as usize
    }

    #[inline(always)]
    pub fn lo(&self) -> u32 {
        self.0.get() & 0x1f
    }
}

impl AsRef<NonZeroU32> for InterruptId {
    #[inline(always)]
    fn as_ref(&self) -> &NonZeroU32 {
        &self.0
    }
}

impl Drop for InterruptId {
    #[inline(always)]
    fn drop(&mut self) {
        // `InterruptId` must only appear from `Plic::next` and utilized in `Plic::complete`
        // otherwise use `InterruptNumber`
        panic!();
    }
}

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InterruptPriority {
    _0,
    _1,
    _2,
    _3,
    _4,
    _5,
    _6,
    _7,
}

impl PlicThresholdClaim {
    #[inline(always)]
    pub fn next(&self) -> Option<InterruptId> {
        self.claim.read()
    }

    #[inline(always)]
    pub fn complete(&self, id: InterruptId) {
        self.claim.write(id);
    }

    #[inline(always)]
    pub fn set_threshold(&self, priority: InterruptPriority) {
        self.threshold.write(priority);
    }
}

impl Plic {
    #[allow(dead_code)]
    #[inline(always)]
    pub fn is_pending(&self, id: &InterruptNumber) -> bool {
        self.pending_bit[id.hi()].read() & (1 << id.lo()) != 0
    }

    #[inline(always)]
    pub fn enable(&self, context_id: usize, id: &InterruptNumber) {
        let Some(enable_bit) = self.enable_bit.get(context_id) else {
            return;
        };
        let reg = &enable_bit[id.hi()];
        let mask = reg.read();
        let mask = mask | (1 << id.lo());
        reg.write(mask);
    }

    #[inline(always)]
    pub fn set_priority(&self, id: &InterruptNumber, priority: InterruptPriority) {
        if let Some(reg) = self.priorities.get(id.as_int() as usize) {
            reg.write(priority as u32)
        }
    }
}
