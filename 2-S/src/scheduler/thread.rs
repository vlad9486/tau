// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use super::space::Root;

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
    Entry(usize),
    Syscall,
    Interrupt,
}

pub(super) struct ActiveWait {
    pub module: usize,
    pub slot: usize,
    pub sequence: usize,
}

pub struct ThreadState {
    pub(super) phase: Phase,
    pub root: Root,
    pub resume: Resume,
    pub message: [usize; 6],
    pub sequence: usize,
    pub(super) wait: Option<ActiveWait>,
}

impl ThreadState {
    pub fn runnable(root: Root, message: [usize; 6], resume: Resume) -> Self {
        ThreadState {
            phase: Phase::Runnable,
            root,
            resume,
            message,
            sequence: 0,
            wait: None,
        }
    }

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
