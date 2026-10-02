// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{fmt, num::NonZeroUsize};

#[repr(C)]
pub struct Manifest {
    pub this: ModuleId,
    pub entry: Entry,
    pub dependencies: &'static [ModuleId],
    pub mapped_regions: &'static [MappedRegion],
}

pub type Entry = extern "C" fn(usize, usize, usize, usize, usize, usize) -> !;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ModuleId {
    pub version: (u16, u16),
    pub name: [u8; 20],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MappedRegion {
    pub phys_start: Option<NonZeroUsize>,
    pub virtual_start: isize,
    pub pages: usize,
    pub write: bool,
}

impl MappedRegion {
    pub const fn stack(size: usize) -> Self {
        MappedRegion {
            // physical address is allocated dynamically
            phys_start: None,
            virtual_start: -(size as isize),
            pages: size >> 12,
            write: true,
        }
    }
}

#[must_use]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Timeout,
    Interrupt {
        id: u16,
    },
    /// Entry-point dispatch, not a wakeup of a suspended context.
    Invocation {
        arg: u16,
    },
    /// A coalesced readiness notification; inspect shared state after waking.
    Notified,
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => write!(f, "timeout"),
            Self::Interrupt { id } => write!(f, "int={id:04x}"),
            Self::Invocation { arg } => write!(f, "invocation arg={arg}"),
            Self::Notified => write!(f, "notified"),
        }
    }
}

impl Event {
    pub const fn encode(self) -> usize {
        match self {
            Self::Timeout => 0,
            Self::Interrupt { id } => ((id as usize) << 16) + 1,
            Self::Invocation { arg } => ((arg as usize) << 16) + 2,
            Self::Notified => 3,
        }
    }

    pub const fn decode(a0: usize) -> Result<Self, usize> {
        let arg = ((a0 & 0xffff0000) >> 16) as u16;
        match a0 & 0b111 {
            0 if a0 == 0 => Ok(Self::Timeout),
            1 if a0 == ((arg as usize) << 16) + 1 => Ok(Self::Interrupt { id: arg }),
            2 if a0 == ((arg as usize) << 16) + 2 => Ok(Self::Invocation { arg }),
            3 if a0 == 3 => Ok(Self::Notified),
            _ => Err(a0),
        }
    }
}

/// The supervisor interface. Additional machine words are passed in registers.
/// Shared mappings are established declaratively by the loader, not by calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// Save the caller and transfer this thread to another module context's
    /// entry point. The caller remains suspended until `Respond` restores it.
    /// This does not resume a waiting context or create a thread.
    /// `slot` selects a dependency; `arg` is a small dispatch argument.
    Invoke { slot: u16, arg: u16 },
    /// Return this thread to its previous module context with the response code
    /// and message. Never returns in the callee; the thread continues in its
    /// caller. Zero is success. No invocation ID is needed.
    Respond { code: u16 },
    /// Spawns a thread with its own context in the same ELF module.
    /// The new thread will run in the specified entry point with the given message.
    ///
    /// # Parameters
    /// - `entry`: The entry point of the new thread.
    Spawn { entry: usize },
    /// Exit the current thread with the given message. Never returns, the thread cease to exist.
    Exit,
    /// Blocks until the specified thread completes execution and returns a message.
    /// No message required.
    ///
    /// # Parameters
    /// - `thread_id`: The ID of the thread to wait.
    Join { thread_id: u16 },
    /// Map the physical memory on the virtual address space of the thread.
    /// The message may provide a desired physical address or zero.
    /// If zero specified the system will allocate the physical address.
    /// If physical address specified the system will check permission to access the address.
    /// The message also must provide a desired virtual address and a number of pages.
    Map,
    /// Unmap the physical memory from the virtual address space of the thread.
    /// The message must provide the virtual address and the number of pages.
    Unmap,
    /// Transitional bootstrap operation: wait for an external interrupt or
    /// deadline (a1, zero for none). Blocks the hart, not a scheduled thread.
    /// Superseded by a registered waiter bound to the hart's external interrupt.
    WaitExternal,
    /// Wait on this thread's registered waiter (ID in a1, deadline in a2;
    /// zero means no deadline). Consume pending readiness or park this thread.
    /// Resume after this call in the same context. Return status in a0 and an
    /// encoded `Event` in a1. A timeout does not consume a pending notification.
    Wait,
    /// Notify a waiter in the current module (ID in a1). Coalesce readiness and
    /// make its thread runnable if parked; continue executing in this context.
    /// Notification before wait is remembered. No handler is invoked.
    Notify,
    /// Emit a diagnostic checkpoint through the supervisor console.
    Debug,
}

impl Call {
    #[inline]
    pub const fn encode(self) -> usize {
        match self {
            Self::Invoke { slot, arg } => {
                ((arg as usize) << 16) + (((slot & 0xfff) as usize) << 4) + 0b0001
            }
            Self::Respond { code } => ((code as usize) << 16) + 0b0101,
            Self::Spawn { entry } => entry,
            Self::Exit => 0b1001,
            Self::Join { thread_id } => ((thread_id as usize) << 16) + (1 << 4) + 0b1001,
            Self::Map => (2 << 4) + 0b1001,
            Self::Unmap => (3 << 4) + 0b1001,
            Self::WaitExternal => (4 << 4) + 0b1001,
            Self::Debug => (5 << 4) + 0b1001,
            Self::Wait => (6 << 4) + 0b1001,
            Self::Notify => (7 << 4) + 0b1001,
        }
    }

    #[inline]
    pub const fn decode(a0: usize) -> Result<Self, usize> {
        if a0 & 0b0001 == 0 {
            let entry = a0;
            Ok(Self::Spawn { entry })
        } else {
            // The old share/accept flag is reserved; do not decode old calls.
            if a0 & 0b0010 != 0 {
                return Err(a0);
            }
            let discriminant = (a0 & 0b1100) >> 2;
            let id = ((a0 & 0xfff0) >> 4) as u16;
            let arg = ((a0 & 0xffff0000) >> 16) as u16;
            match discriminant {
                0b00 => Ok(Self::Invoke { slot: id, arg }),
                0b01 if id == 0 => Ok(Self::Respond { code: arg }),
                0b10 => match id {
                    0 => Ok(Self::Exit),
                    1 => {
                        let thread_id = arg;
                        Ok(Self::Join { thread_id })
                    }
                    2 => Ok(Self::Map),
                    3 => Ok(Self::Unmap),
                    4 => Ok(Self::WaitExternal),
                    5 => Ok(Self::Debug),
                    6 => Ok(Self::Wait),
                    7 => Ok(Self::Notify),
                    _ => Err(a0),
                },
                _ => Err(a0),
            }
        }
    }
}

#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitError {
    Unsupported = 1,
    InvalidWaiter = 2,
    AccessDenied = 3,
    Busy = 4,
    OutOfMemory = 5,
}
