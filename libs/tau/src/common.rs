// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{fmt, sync::atomic::AtomicBool, mem};

#[repr(C)]
pub struct Manifest {
    pub this: ModuleId,
    pub entry: Entry,
    pub stack_pages: usize,
    pub dependencies: &'static [ModuleId],
    pub mapped_regions: &'static [MappedRegion],
    pub wait_slots: &'static [WaitSlot],
}

// The manifest is immutable image metadata. Its raw address is validated by the
// supervisor; sharing the manifest never dereferences that address in Rust.
unsafe impl Sync for Manifest {}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawManifest {
    pub this: ModuleId,
    pub entry: usize,
    pub stack_pages: usize,
    pub dependencies: *const [ModuleId],
    pub mapped_regions: *const [MappedRegionRaw],
    pub wait_slots: *const [WaitSlot],
}

const _: () = {
    assert!(size_of::<Manifest>() == size_of::<RawManifest>());
    assert!(align_of::<Manifest>() == align_of::<RawManifest>());
    assert!(mem::offset_of!(Manifest, this) == mem::offset_of!(RawManifest, this));
    assert!(mem::offset_of!(Manifest, entry) == mem::offset_of!(RawManifest, entry));
    assert!(mem::offset_of!(Manifest, stack_pages) == mem::offset_of!(RawManifest, stack_pages));
    assert!(mem::offset_of!(Manifest, dependencies) == mem::offset_of!(RawManifest, dependencies));
    assert!(
        mem::offset_of!(Manifest, mapped_regions) == mem::offset_of!(RawManifest, mapped_regions)
    );
    assert!(mem::offset_of!(Manifest, wait_slots) == mem::offset_of!(RawManifest, wait_slots));
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MappedRegion {
    /// Device identifier or DTB path, e.g. "/soc/sdio1"
    pub dtb_path: &'static str,
    /// Destination virtual address in the module's address space
    pub ptr: *const (),
    /// Number of 4 KiB pages required
    pub pages: usize,
}

unsafe impl Send for MappedRegion {}
unsafe impl Sync for MappedRegion {}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MappedRegionRaw {
    pub dtb_path: *const str,
    pub ptr: *const (),
    pub pages: usize,
}

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

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryZone {
    Normal = 0,
    Dma = 1,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageFlags(u8);

impl PageFlags {
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXEC: Self = Self(1 << 2);
    pub const RW: Self = Self(Self::READ.0 | Self::WRITE.0);
    pub const RX: Self = Self(Self::READ.0 | Self::EXEC.0);

    pub const fn from_raw(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn as_raw(self) -> u8 {
        self.0
    }

    pub const fn bits(self) -> usize {
        self.0 as usize
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
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
    /// and message. Of keep is false never returns in the callee; the thread
    /// continues in its caller. Otherwise, the continuation is kept and a new
    /// invocation will continue in the callee at the stored continuation.
    Respond { code: u16, keep: bool },
    /// Wait on a reserved module-global slot: a1 = slot address, a2 = absolute
    /// time-counter deadline (zero for none). Return status in a0, Event in a1.
    Wait,
    /// Notify a slot in the current module (a1 = slot address). Coalesces
    /// readiness and schedules a parked client; the notifying thread continues.
    Notify,
    /// Spawns a thread with its own context in the same ELF module.
    /// The new thread will run in the specified entry point with the given message.
    ///
    /// # Parameters
    /// - `entry`: The entry point of the new thread.
    Spawn { entry: usize },
    /// Exit the current thread with the given message. Never returns, the thread cease to exist.
    Exit,
    /// Loader-only: allocate 2^order contiguous physical frames from `zone`.
    /// Return status in a0 and the physical address in a1.
    AllocPhysical { order: u8, zone: MemoryZone },
    /// Loader-only: release 2^order contiguous physical frames at a1 back to `zone`.
    FreePhysical { order: u8, zone: MemoryZone },
    /// Map physical or anonymous memory into a target module's virtual address space:
    /// a1 = target handle (0 for self), a2 = physical address (0 for anonymous),
    /// a3 = virtual address, a4 = pages, a5 = PageFlags.
    Map,
    /// Unmap virtual memory from a target module's virtual address space:
    /// a1 = target handle (0 for self), a2 = virtual address, a3 = pages.
    Unmap,
    /// Loader-only: create an empty module template with a1 = entry,
    /// a2 = stack_pages, a3 = wait_slots_addr, a4 = wait_slots_count.
    /// Return status in a0 and the new ModuleHandle in a1.
    CreateModule,
    /// Loader-only: bind dependency handle a2 under parent handle a1.
    /// Return status in a0 and a fresh parent-local slot in a1.
    BindDependency,
    /// Loader-only: start secondary hart a1 in the current module.
    StartHart,
    /// Emit a diagnostic checkpoint through the supervisor console.
    Debug,
}

impl Call {
    // WARNING: invoke/respond assembly code rely on this encoding
    // check all `naked_asm!` and `asm!` before modify this
    #[inline]
    pub const fn encode(self) -> usize {
        match self {
            Self::Invoke { slot, arg } => {
                ((arg as usize) << 16) + (((slot & 0xfff) as usize) << 4) + 0b0001
            }
            Self::Respond { code, keep } => {
                let flags = if keep { 1 } else { 0 };
                ((code as usize) << 16) + (flags << 4) + 0b0101
            }
            Self::Wait => (6 << 4) + 0b1001,
            Self::Notify => (7 << 4) + 0b1001,
            Self::Spawn { entry } => entry,
            Self::Exit => 0b1001,
            Self::AllocPhysical { order, zone } => {
                (((order as usize) | ((zone as usize) << 8)) << 16) + (1 << 4) + 0b1001
            }
            Self::Map => (2 << 4) + 0b1001,
            Self::Unmap => (3 << 4) + 0b1001,
            Self::FreePhysical { order, zone } => {
                (((order as usize) | ((zone as usize) << 8)) << 16) + (4 << 4) + 0b1001
            }
            Self::CreateModule => (8 << 4) + 0b1001,
            Self::BindDependency => (9 << 4) + 0b1001,
            Self::StartHart => (10 << 4) + 0b1001,
            Self::Debug => (5 << 4) + 0b1001,
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
                0b01 if id <= 1 => {
                    let keep = id != 0;
                    Ok(Self::Respond { code: arg, keep })
                }
                0b10 => match id {
                    6 => Ok(Self::Wait),
                    7 => Ok(Self::Notify),
                    0 => Ok(Self::Exit),
                    1 => {
                        let order = (arg & 0xff) as u8;
                        let zone = match arg >> 8 {
                            0 => MemoryZone::Normal,
                            1 => MemoryZone::Dma,
                            _ => return Err(a0),
                        };
                        Ok(Self::AllocPhysical { order, zone })
                    }
                    2 => Ok(Self::Map),
                    3 => Ok(Self::Unmap),
                    4 => {
                        let order = (arg & 0xff) as u8;
                        let zone = match arg >> 8 {
                            0 => MemoryZone::Normal,
                            1 => MemoryZone::Dma,
                            _ => return Err(a0),
                        };
                        Ok(Self::FreePhysical { order, zone })
                    }
                    8 => Ok(Self::CreateModule),
                    9 => Ok(Self::BindDependency),
                    10 => Ok(Self::StartHart),
                    5 => Ok(Self::Debug),
                    _ => Err(a0),
                },
                _ => Err(a0),
            }
        }
    }
}
