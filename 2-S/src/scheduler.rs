// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

/// Local thread metadata, mapped into each context executing this thread.
/// Parent/child lifetime information belongs here when spawn/join are implemented.
/// There is no separate thread control block or collection of threads here.
#[repr(C)]
pub struct Thread {
    hart_id: usize,
    pub pending_interrupts: usize,
}

impl Thread {
    pub fn set_hart_id(&mut self, hart_id: usize) {
        self.hart_id = hart_id;
    }

    pub fn hart_id(&self) -> usize {
        self.hart_id
    }
}

/// System-wide scheduling state; the global registry of tasks belongs here.
#[repr(C)]
pub struct Scheduler {
    asid_bitmap: [u32; 2048],
}
