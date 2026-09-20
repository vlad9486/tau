mod atomic;
mod util;
mod bitfield;

mod id;
pub use self::id::{FrameId, RowId};

mod lower;
pub use self::lower::{Lower, Allocator};

/// Number of huge frames in tree
pub const TREE_HUGE: usize = 8;
/// Number of small frames in tree
pub const TREE_FRAMES: usize = TREE_HUGE << HUGE_ORDER;
/// Order of an entire tree
pub const TREE_ORDER: usize = TREE_FRAMES.ilog2() as usize;
/// Order for huge frames
pub const HUGE_ORDER: usize = 9;
/// Number of small frames in huge frame
pub const HUGE_FRAMES: usize = 1 << HUGE_ORDER;
/// Maximum order the llfree supports
pub const MAX_ORDER: usize = HUGE_ORDER + 1;
/// Bit size of the atomic ints that comprise the bitfields
pub const BITFIELD_ROW: usize = 64;

pub const ROWS: usize = HUGE_FRAMES / BITFIELD_ROW;

/// Maximum number of frames accepted by [`Allocator::new`].
///
/// This bound guarantees that every metadata index and even statistics over
/// maximally corrupted `u16` counters fit in `usize`.
pub const MAX_FRAMES: usize = (usize::MAX / u16::MAX as usize / TREE_HUGE) * TREE_FRAMES;

const _: () = assert!(usize::BITS <= u64::BITS);
const _: () = assert!(MAX_FRAMES <= usize::MAX - (TREE_FRAMES - 1));
const _: () =
    assert!(MAX_FRAMES.div_ceil(TREE_FRAMES) * TREE_HUGE <= usize::MAX / u16::MAX as usize);

/// Number of retries if an atomic operation fails.
const RETRIES: usize = 4;

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
    /// _
    FailedUndoToggle = 5,
    FailedUndoSearch = 6,
    UndoFailed = 7,
    InvalidArgument = 8,
    OrderNotSuported = 9,
    FailedToIncrement = 10,
}

/// Allocation statistics of allocator
#[derive(Debug, Default)]
pub struct Stats {
    /// Number of free frames
    pub free_frames: usize,
    /// Number of entirely free huge frames
    pub free_huge: usize,
    /// Number of entirely free trees
    pub free_trees: usize,
}
