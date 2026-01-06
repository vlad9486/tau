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
#[derive(Clone, Debug)]
pub enum Event {
    Timeout,
    Interrupt { id: u16 },
    Invocation { inv: u16, share: bool, arg: u16 },
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => write!(f, "timeout"),
            Self::Interrupt { id } => write!(f, "int={id:04x}"),
            Self::Invocation { inv, share, arg } => {
                write!(f, "inv={inv}, share={share}, arg={arg}")
            }
        }
    }
}

impl Event {
    pub const fn encode(self) -> usize {
        match self {
            Self::Timeout => 0,
            Self::Interrupt { id } => ((id as usize) << 16) + 1,
            Self::Invocation { inv, share, arg } => {
                ((arg as usize) << 16)
                    + (((inv & 0xfff) as usize) << 4)
                    + ((share as usize) << 3)
                    + 2
            }
        }
    }

    pub const fn decode(a0: usize) -> Result<Self, usize> {
        let inv = ((a0 & 0xfff0) >> 4) as u16;
        let arg = ((a0 & 0xffff0000) >> 16) as u16;
        let share = (a0 & 0b1000) != 0;
        match a0 & 0b111 {
            0 => Ok(Self::Timeout),
            1 => Ok(Self::Interrupt { id: arg }),
            2 => Ok(Self::Invocation { inv, share, arg }),
            _ => Err(a0),
        }
    }
}

pub fn event_with(event: &mut Option<Event>, id: Option<u32>) -> Option<Event> {
    if let Some(id) = id {
        let id = id as u16;
        return Some(Event::Interrupt { id });
    }
    event.take()
}

/// The interface of the supervisor
pub enum Call {
    /// Invokes a module by spawning an invocation at its entry point
    /// and blocking until it responds.
    ///
    /// If `share` parameter is true, share memory with a module.
    /// The message must specify the address and size to share.
    ///
    /// # Parameters
    /// - `slot`: The slot of the module in the dependencies table to invoke.
    /// - `share`: Whether share memory or not.
    /// - `arg`: A small argument passed to the invoked module.
    Invoke { slot: u16, share: bool, arg: u16 },
    /// Sends a response to the caller that invoked this module.
    /// The current thread cease to exist and execution continues at caller's point.
    ///
    /// Accept or reject the shared memory.
    /// The message must specify the address where to put the shared memory.
    ///
    /// # Parameters
    /// - `inv`: The ID of the invocation to respond to.
    /// - `accept`: Whether accept or reject shared memory.
    /// - `code`: The response error code to send back.
    Respond { inv: u16, accept: bool, code: u16 },
    /// Spawns a thread within the current module.
    /// The new thread will run in the specified entry point with the given message.
    ///
    /// # Parameters
    /// - `entry`: The entry point of the new thread.
    Spawn { entry: usize },
    /// Exit the current thread. Never returns, the thread cease to exist.
    /// No message required.
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
    /// Wait an external interrupt. The message may provide a timeout. Privileged.
    Wait,
}

impl Call {
    #[inline]
    pub const fn encode(self) -> usize {
        match self {
            Self::Invoke { slot, share, arg } => {
                let share = if share { 1 } else { 0 };
                ((arg as usize) << 16) + (((slot & 0xfff) as usize) << 4) + (share << 1) + 0b0001
            }
            Self::Respond { inv, accept, code } => {
                let accept = if accept { 1 } else { 0 };
                ((code as usize) << 16) + (((inv & 0xfff) as usize) << 4) + (accept << 1) + 0b0101
            }
            Self::Spawn { entry } => entry,
            Self::Exit => 0b1001,
            Self::Join { thread_id } => ((thread_id as usize) << 16) + (1 << 4) + 0b1001,
            Self::Map => (2 << 4) + 0b1001,
            Self::Unmap => (3 << 4) + 0b1001,
            Self::Wait => (4 << 4) + 0b1001,
        }
    }

    #[inline]
    pub const fn decode(a0: usize) -> Result<Self, usize> {
        if a0 & 0b0001 == 0 {
            let entry = a0;
            Ok(Self::Spawn { entry })
        } else {
            let flag = ((a0 & 0b0010) >> 1) != 0;
            let discriminant = (a0 & 0b1100) >> 2;
            let id = ((a0 & 0xfff0) >> 4) as u16;
            let arg = ((a0 & 0xffff0000) >> 16) as u16;
            match discriminant {
                0b00 => {
                    let slot = id;
                    let share = flag;
                    Ok(Self::Invoke { slot, share, arg })
                }
                0b01 => {
                    let inv = id;
                    let accept = flag;
                    let code = arg;
                    Ok(Self::Respond { inv, accept, code })
                }
                0b10 => match id {
                    0 => Ok(Self::Exit),
                    1 => {
                        let thread_id = id;
                        Ok(Self::Join { thread_id })
                    }
                    2 => Ok(Self::Map),
                    3 => Ok(Self::Unmap),
                    4 => Ok(Self::Wait),
                    _ => Err(a0),
                },
                _ => Err(a0),
            }
        }
    }
}
