use core::fmt;

use super::{HUGE_FRAMES, BITFIELD_ROW, TREE_FRAMES, TREE_HUGE, ROWS};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RowId(pub usize);

impl RowId {
    pub const fn as_frame(self) -> Option<FrameId> {
        match self.0.checked_mul(BITFIELD_ROW) {
            Some(frame) => Some(FrameId(frame)),
            None => None,
        }
    }

    pub const fn as_huge(self) -> HugeId {
        HugeId(self.0 / ROWS)
    }

    pub const fn as_tree(self) -> TreeId {
        TreeId(self.0 / (TREE_FRAMES / BITFIELD_ROW))
    }

    pub const fn huge_idx(self) -> usize {
        self.0 % ROWS
    }

    #[allow(unused)]
    pub const fn into_bits(self) -> u64 {
        self.0 as u64
    }

    #[allow(unused)]
    pub const fn from_bits(bits: u64) -> Option<Self> {
        if bits > usize::MAX as u64 {
            None
        } else {
            Some(Self(bits as usize))
        }
    }
}

impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rx{:x}", self.0)
    }
}

impl fmt::Debug for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TreeId(pub usize);

impl TreeId {
    pub const fn as_frame(self) -> Option<FrameId> {
        match self.0.checked_mul(TREE_FRAMES) {
            Some(frame) => Some(FrameId(frame)),
            None => None,
        }
    }

    pub const fn as_huge(self) -> Option<HugeId> {
        match self.0.checked_mul(TREE_HUGE) {
            Some(huge) => Some(HugeId(huge)),
            None => None,
        }
    }

    pub const fn as_row(self) -> Option<RowId> {
        match self.0.checked_mul(TREE_FRAMES / BITFIELD_ROW) {
            Some(row) => Some(RowId(row)),
            None => None,
        }
    }

    pub const fn from_bits(value: u64) -> Option<Self> {
        if value > usize::MAX as u64 {
            None
        } else {
            Some(Self(value as usize))
        }
    }

    pub const fn into_bits(self) -> u64 {
        self.0 as _
    }
}

impl fmt::Display for TreeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "T{}", self.0)
    }
}

impl fmt::Debug for TreeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameId(pub usize);

impl FrameId {
    pub const fn into_bits(self) -> u64 {
        self.0 as u64
    }
    pub const fn from_bits(bits: u64) -> Option<Self> {
        if bits > usize::MAX as u64 {
            None
        } else {
            Some(Self(bits as usize))
        }
    }

    pub const fn as_tree(self) -> TreeId {
        TreeId(self.0 / TREE_FRAMES)
    }

    pub const fn as_huge(self) -> HugeId {
        HugeId(self.0 / HUGE_FRAMES)
    }

    pub const fn as_row(self) -> RowId {
        RowId(self.0 / BITFIELD_ROW)
    }

    pub const fn row_bit_idx(self) -> usize {
        self.0 % BITFIELD_ROW
    }

    pub const fn is_aligned(self, order: usize) -> bool {
        if order >= usize::BITS as usize {
            return false;
        }
        match 1usize.checked_shl(order as u32) {
            Some(size) => self.0 & (size - 1) == 0,
            None => false,
        }
    }
}

impl fmt::Display for FrameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fx{:x}", self.0)
    }
}

impl fmt::Debug for FrameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct HugeId(pub usize);

impl HugeId {
    pub const fn as_frame(self) -> Option<FrameId> {
        match self.0.checked_mul(HUGE_FRAMES) {
            Some(frame) => Some(FrameId(frame)),
            None => None,
        }
    }

    pub const fn as_tree(self) -> TreeId {
        TreeId(self.0 / TREE_HUGE)
    }

    pub const fn as_row(self) -> Option<RowId> {
        match self.0.checked_mul(ROWS) {
            Some(row) => Some(RowId(row)),
            None => None,
        }
    }

    pub fn child_idx(self) -> usize {
        self.0 % TREE_HUGE
    }
}

impl fmt::Display for HugeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hx{:x}", self.0)
    }
}

impl fmt::Debug for HugeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
