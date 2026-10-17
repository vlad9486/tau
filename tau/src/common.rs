// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{fmt, num::NonZeroUsize, sync::atomic::AtomicBool, mem};

#[repr(C)]
pub struct Manifest {
    pub this: ModuleId,
    pub entry: Entry,
    pub dependencies: &'static [ModuleId],
    pub mapped_regions: &'static [MappedRegion],
    pub wait_slots: &'static [WaitSlot],
}

// The manifest is immutable image metadata. Its raw address is validated by the
// supervisor; sharing the manifest never dereferences that address in Rust.
unsafe impl Sync for Manifest {}

/// One module-global reservation. Shared request data needs its own synchronization.
#[repr(C, align(64))]
pub struct WaitSlot {
    pub user_data: [u8; 63],
    pub occupied: AtomicBool,
}

impl WaitSlot {
    pub const fn new() -> Self {
        Self {
            user_data: [0; 63],
            occupied: AtomicBool::new(false),
        }
    }
}

impl Default for WaitSlot {
    fn default() -> Self {
        Self::new()
    }
}

const _: () = {
    assert!(size_of::<WaitSlot>() == 64);
    assert!(mem::offset_of!(WaitSlot, occupied) == 63);
};

#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnError {
    InvalidEntry = 1,
    OutOfMemory = 2,
    NoSlots = 3,
}

pub type Entry = extern "C" fn(usize, usize, usize, usize, usize, usize) -> !;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ModuleId {
    pub version: (u16, u16),
    pub name: [u8; 20],
}

/// Boot-lifetime handle to a prepared module's physical descriptor page.
/// The trusted loader must use supervisor-issued handles and keep bindings
/// acyclic. This is independent of the manifest's name and version.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleHandle(usize);

impl ModuleHandle {
    /// The bootstrap module. Valid only as a dependency's parent.
    pub const ROOT: Self = Self(0);

    /// Decode a syscall handle. This does not validate descriptor membership.
    pub const fn from_raw(value: usize) -> Self {
        Self(value)
    }

    /// The physical descriptor page number, or zero for the bootstrap module.
    pub const fn as_raw(self) -> usize {
        self.0
    }
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
    /// A coalesced readiness notification; inspect shared state after waking.
    Notified {
        arg: u16,
    },
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => write!(f, "timeout"),
            Self::Notified { arg } => write!(f, "notification arg={arg}"),
        }
    }
}

impl Event {
    pub const fn encode(self) -> usize {
        match self {
            Self::Timeout => 0,
            Self::Notified { arg } => ((arg as usize) << 16) + 1,
        }
    }

    pub const fn decode(a0: usize) -> Result<Self, usize> {
        let arg = ((a0 & 0xffff0000) >> 16) as u16;
        match a0 & 0b111 {
            0 => Ok(Self::Timeout),
            1 if a0 == ((arg as usize) << 16) + 1 => Ok(Self::Notified { arg }),
            _ => Err(a0),
        }
    }
}

/// The supervisor interface. Additional machine words are passed in registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    /// Save the caller and transfer this thread to another module context's
    /// entry point. The caller remains suspended until `Respond` restores it.
    /// This does not resume a waiting context or create a thread.
    /// `slot` selects a dependency; `arg` is a small dispatch argument.
    Invoke { slot: u16, arg: u16 },
    /// Return this thread to its previous module context with the response code
    /// and message. Never returns in the callee; the thread continues in its
    /// caller. Zero is success.
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
    /// Wait on a reserved module-global slot: a1 = slot address, a2 = absolute
    /// time-counter deadline (zero for none). Return status in a0, Event in a1.
    Wait,
    /// Notify a slot in the current module (a1 = slot address). Coalesces
    /// readiness and schedules a parked client; the notifying thread continues.
    Notify,
    /// Emit a diagnostic checkpoint through the supervisor console.
    Debug,
    /// Loader-only: prepare an ELF image at (a1, a2) under parent handle a3.
    /// Return status in a0, a parent-local slot in a1, and the new handle in a2.
    PrepareModule,
    /// Loader-only: bind dependency handle a2 under parent handle a1.
    /// Return status in a0 and a fresh parent-local slot in a1.
    BindDependency,
}

impl Call {
    // WARNING: some assembly code might rely on this encoding
    // check all `naked_asm!` and `asm!` before modify this
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
            Self::Debug => (5 << 4) + 0b1001,
            Self::Wait => (6 << 4) + 0b1001,
            Self::Notify => (7 << 4) + 0b1001,
            Self::PrepareModule => (8 << 4) + 0b1001,
            Self::BindDependency => (9 << 4) + 0b1001,
        }
    }

    #[inline(always)]
    pub const fn decode(a0: usize) -> Result<Self, usize> {
        if a0 & 0b0001 == 0 {
            let entry = a0;
            Ok(Self::Spawn { entry })
        } else {
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
                    5 => Ok(Self::Debug),
                    6 => Ok(Self::Wait),
                    7 => Ok(Self::Notify),
                    8 => Ok(Self::PrepareModule),
                    9 => Ok(Self::BindDependency),
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

/// Construction/invocation status. Module authority is granted by bootstrap,
/// never inferred from an ELF's name or inherited from the invoking thread.
#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleError {
    AccessDenied = 1,
    InvalidImage = 2,
    OutOfMemory = 3,
    NoSlots = 4,
    InvalidDependency = 5,
}
