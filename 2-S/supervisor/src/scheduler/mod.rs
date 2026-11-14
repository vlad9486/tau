// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Multi-hart scheduling with hart-pinned thread contexts. Thread state stays
//! in the global __SCHEDULER mapping; per-hart FIFO queues hold thread IDs.

use super::{space, csr, sbi, matter, state::Context};

mod thread;
mod runtime;
mod operations;

pub use self::thread::{ThreadState, Resume as ReturnKind};
pub use self::runtime::{
    Scheduler, Dispatch, preempt, dispatch, notify_interrupt, notify_hardware_interrupt, MAX_HARTS,
};
pub use self::operations::{spawn, start_hart, wait, notify, exit};
