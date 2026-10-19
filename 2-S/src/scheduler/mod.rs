// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Single-active-hart scheduling. Thread state stays in the global __SCHEDULER
//! mapping; the FIFO holds thread IDs.

use super::{cpu, sbi, vmem, waiting, loading, module, state::Context};

mod thread;
mod runtime;
mod operations;

pub use self::thread::{ThreadState, Resume as ReturnKind};
pub use self::runtime::{Scheduler, Dispatch, preempt, dispatch, notify_hardware_interrupt};
pub use self::operations::{spawn, wait, notify, exit};
