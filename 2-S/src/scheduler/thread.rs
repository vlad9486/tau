// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use super::{vmem::Root, waiting::WaitTable};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Running,
    Runnable,
    Parking,
    Parked,
    Exited,
}

#[derive(Clone, Copy)]
pub enum Resume {
    Entry,
    Syscall,
    Interrupt,
}

#[derive(Clone, Copy)]
pub(super) struct ActiveWait {
    pub module: usize,
    pub table: WaitTable,
    pub slot: usize,
    pub sequence: usize,
}

impl ActiveWait {
    pub fn matches(self, other: Self) -> bool {
        self.module == other.module && self.slot == other.slot && self.sequence == other.sequence
    }
}

#[derive(Clone, Copy)]
pub(super) struct ThreadState {
    pub phase: Phase,
    pub root: Root,
    pub resume: Resume,
    pub message: [usize; 6],
    pub sequence: usize,
    pub wait: Option<ActiveWait>,
}

impl ThreadState {
    /// Final departure decision. A completed Parking wait keeps its hart and
    /// returns directly; an outstanding one releases ownership as Parked.
    pub fn commit_parking(&mut self) -> bool {
        debug_assert_eq!(self.phase, Phase::Parking);
        if self.wait.is_none() {
            self.phase = Phase::Running;
            false
        } else {
            self.phase = Phase::Parked;
            true
        }
    }

    /// Completion during Parking preserves running ownership. Only a committed
    /// Parked wait may acquire queue membership through completion.
    pub fn complete(&mut self, message: [usize; 6]) -> bool {
        debug_assert!(matches!(self.phase, Phase::Parking | Phase::Parked));
        self.wait = None;
        self.message = message;
        if self.phase == Phase::Parking {
            false
        } else {
            self.phase = Phase::Runnable;
            true
        }
    }
}

/// Identity/affinity are immutable after publication. All non-bootstrap mutable
/// fields are accessed only under the scheduler lock, including through aliases.
#[repr(C)]
pub struct Thread {
    pub(super) hart_id: usize,
    pub(super) state: ThreadState,
}

impl Thread {
    pub fn hart_id(&self) -> usize {
        self.hart_id
    }

    /// Create thread before forming a shared reference.
    pub fn new(hart: usize, root: Root, message: [usize; 6], resume: Resume) -> Self {
        Thread {
            hart_id: hart,
            state: ThreadState {
                phase: Phase::Runnable,
                root,
                resume,
                message,
                sequence: 0,
                wait: None,
            },
        }
    }
}
