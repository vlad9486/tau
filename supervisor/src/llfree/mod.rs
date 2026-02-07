mod atomic;
mod bitfield;
mod local;
mod lower;
mod trees;
mod util;

mod inner;
pub use self::inner::LLFree;

/// Allocation error
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not enough memory
    Memory = 1,
    /// Failed atomic operation, retry procedure
    Retry = 2,
    /// Invalid address
    Address = 3,
    /// Allocator not initialized or initialization failed
    Initialization = 4,
    /// Exceed reties while putting page
    ExceedReties = 5,
    /// _
    FailedUndoToggle,
    FailedUndoSearch,
    UndoFailed,
    FailedPartialCase,
}

/// Number of huge frames in tree
const TREE_HUGE: usize = 8;
/// Number of small frames in tree
const TREE_FRAMES: usize = TREE_HUGE << HUGE_ORDER;
/// Order for huge frames
const HUGE_ORDER: u32 = 9;
/// Number of small frames in huge frame
const HUGE_FRAMES: usize = 1 << HUGE_ORDER;

/// Number of retries if an atomic operation fails.
const RETRIES: usize = 4;

#[derive(Clone, Copy)]
pub struct Flags {
    pub order: u32,
    pub movable: bool,
}

impl Flags {
    pub fn o(order: u32) -> Self {
        Flags {
            order,
            movable: false,
        }
    }
}

/// Defines if the allocator should be allocated persistently
/// and if it in that case should try to recover from the persistent memory.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Init {
    /// Clear the allocator marking all frames as free
    FreeAll,
    /// Clear the allocator marking all frames as allocated
    AllocAll,
    /// Assume that the allocator is already initialized
    None,
}
