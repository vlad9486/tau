use core::sync::atomic::AtomicU64;

use bitfield_struct::bitfield;

use super::atomic::{Atom, Atomic};
use super::trees::Kind;
use super::TREE_FRAMES;

/// Core-local data
#[derive(Default, Debug)]
pub struct Local {
    /// Reserved trees for each [Kind]
    preferred: [Atom<LocalTree>; Kind::LEN],
    /// Recent frees
    frees: Atom<FreeHistory>,
}

impl Local {
    pub fn preferred(&self, kind: Kind) -> &Atom<LocalTree> {
        &self.preferred[kind as usize]
    }

    /// Add a tree index to the history, returning if there are enough frees
    pub fn frees_push(&self, tree_idx: usize) -> bool {
        let mut success = false;
        let _ = self.frees.fetch_update(|mut v| {
            success = v.push(tree_idx);
            Some(v)
        });
        success
    }
}

/// Local tree copy
#[bitfield(u64)]
#[derive(PartialEq, Eq)]
pub struct LocalTree {
    #[bits(48)]
    pub frame: usize,
    #[bits(15)]
    pub free: usize,
    /// Reserved for present bit...
    pub present: bool,
}

impl Atomic for LocalTree {
    type I = AtomicU64;
}

impl LocalTree {
    pub fn with(frame: usize, free: usize) -> Self {
        unsafe {
            Self::new()
                .with_frame_checked(frame)
                .unwrap_unchecked()
                .with_free_checked(free)
                .unwrap_unchecked()
        }
        .with_present(true)
    }

    pub fn none() -> Self {
        Self::new().with_present(false)
    }

    pub fn dec(self, free: usize) -> Option<Self> {
        if self.present() {
            Some(self.with_free(self.free().checked_sub(free)?))
        } else {
            None
        }
    }

    pub fn inc(self, frame: usize, free: usize) -> Option<Self> {
        if self.present() && self.frame() / TREE_FRAMES == frame / TREE_FRAMES {
            debug_assert!(self.free() + free <= TREE_FRAMES);
            Some(unsafe {
                self.with_free_checked(self.free() + free)
                    .unwrap_unchecked()
            })
        } else {
            None
        }
    }

    pub fn set_start(self, frame: usize, force: bool) -> Option<Self> {
        if force || (self.present() && self.frame() / TREE_FRAMES == frame / TREE_FRAMES) {
            Some(self.with_frame(frame))
        } else {
            None
        }
    }

    pub fn steal(self, free: usize) -> Option<Self> {
        if self.present() && self.free() >= free {
            Some(Self::none())
        } else {
            None
        }
    }
}

#[bitfield(u64)]
pub struct FreeHistory {
    #[bits(48)]
    idx: usize,
    #[bits(16)]
    counter: usize,
}

impl FreeHistory {
    /// Threshold for the number of frees after which a tree is reserved
    const F: usize = 4;

    /// Add a tree index to the history, returning if there are enough frees
    pub fn push(&mut self, tree_idx: usize) -> bool {
        if self.idx() == tree_idx {
            if self.counter() >= Self::F {
                return true;
            }
            self.set_counter(self.counter() + 1);
        } else {
            unsafe { self.set_idx_checked(tree_idx).unwrap_unchecked() };
            self.set_counter(0);
        }
        false
    }
}

impl Atomic for FreeHistory {
    type I = AtomicU64;
}
