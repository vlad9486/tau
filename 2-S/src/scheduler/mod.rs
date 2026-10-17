// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Single-active-hart scheduling. Thread state stays in retained __THREAD
//! backing; the registry and FIFO hold protected references, not another state.

use super::{cpu, sbi, vmem, waiting, loading, module, state::Context};

mod thread;
mod runtime;
mod operations;

pub use self::thread::{Thread, Resume as ReturnKind};
pub use self::runtime::{Scheduler, Dispatch, preempt, dispatch, notify_hardware_interrupt};
pub use self::operations::{spawn, wait, notify, exit};
