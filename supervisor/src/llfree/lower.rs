//! Lower allocator implementations

use core::hint;
use core::sync::atomic::AtomicU16;

use super::atomic::{Atom, AtomArray, Atomic};
use super::util::{align_down, Align};
use super::{Error, Init, HUGE_FRAMES, HUGE_ORDER, RETRIES, TREE_FRAMES, TREE_HUGE};

const CHILDREN: usize = HUGE_FRAMES / super::bitfield::Bitfield::<1>::ENTRY_BITS;
pub type Bitfield = super::bitfield::Bitfield<CHILDREN>;

/// Lower-level frame allocator.
///
/// This level implements the actual allocation/free operations.
/// Each allocation/free is limited to a chunk of [LowerAlloc::N] frames.
///
/// Here the bitfields are 512 bit large -> strong focus on huge frames.
/// Upon that is a table for each tree, with an entry per bitfield.
///
/// The parameter `HP` configures the number of table entries (huge frames per tree).
/// It has to be a multiple of 2!
///
/// ## Memory Layout
/// **persistent:**
/// ```text
/// NVRAM: [ Frames | Bitfields | Tables | Zone ]
/// ```
/// **volatile:**
/// ```text
/// RAM: [ Frames ], Bitfields and Tables are allocated elsewhere
/// ```
#[derive(Default, Debug)]
pub struct Lower<'a> {
    len: usize,
    bitfields: &'a [Align<Bitfield>],
    children: &'a [Align<[Atom<HugeEntry>; TREE_HUGE]>],
}

const _: () = assert!(TREE_HUGE < (1 << (u16::BITS - HUGE_ORDER)));

impl<'a> Lower<'a> {
    /// Create a new lower allocator.
    pub fn new(
        frames: usize,
        init: Init,
        bitfields: &'a [Align<Bitfield>],
        children: &'a [Align<[Atom<HugeEntry>; TREE_HUGE]>],
    ) -> Result<Self, Error> {
        let alloc = Self {
            len: frames,
            bitfields,
            children,
        };

        match init {
            Init::FreeAll => alloc.free_all(),
            Init::AllocAll => alloc.reserve_all(),
            Init::Recover(false) | Init::None => {} // skip, assuming everything is valid
            Init::Recover(true) => alloc.recover(),
        }
        Ok(alloc)
    }

    pub fn frames(&self) -> usize {
        self.len
    }

    fn bitfield(&self, frame: usize) -> &Bitfield {
        unsafe { self.bitfields.get_unchecked(frame / Bitfield::LEN) }
    }

    fn child(&self, frame: usize) -> &[Atom<HugeEntry>; TREE_HUGE] {
        unsafe { self.children.get_unchecked(frame / TREE_FRAMES) }
    }

    /// Recovers the data structures for the [LowerAlloc::N] sized chunk at `start`.
    /// This corrects any data corrupted by a crash.
    pub fn recover(&self) {
        for (i, table) in self.children.iter().enumerate() {
            for (j, a_entry) in table.iter().enumerate() {
                let start = i * TREE_FRAMES + j * Bitfield::LEN;
                let entry = a_entry.load();

                if entry.huge() {
                    // Check that underlying bitfield is empty
                    let p = self.bitfield(start).count_zeros();
                    if p != Bitfield::LEN {
                        // log::warn!("Invalid L2 start=0x{start:x} i{i}: h != {p}");
                        self.bitfield(start).fill(false);
                    }
                } else {
                    // Check the bitfield has the same number of zero bits
                    let zeros = self.bitfield(start).count_zeros();
                    if entry.free() != zeros {
                        // log::warn!(
                        //     "Invalid L2 start=0x{start:x} i{i}: {} != {zeros}",
                        //     entry.free()
                        // );
                        a_entry.store(HugeEntry::new_free(zeros));
                    }
                }
            }
        }
    }

    /// Return the number of free frames in the tree at `start`.
    pub fn free_in_tree(&self, start: usize) -> usize {
        debug_assert!(start < self.frames());
        let mut free = 0;
        for entry in self.child(start).iter() {
            free += entry.load().free();
        }
        free
    }

    /// Try allocating a new `frame` in the [LowerAlloc::N] sized chunk at `start`.
    ///
    /// Returns the allocated frame and whether a new huge frame was fragmented.
    pub fn get(&self, start: usize, order: u32) -> Result<(usize, bool), Error> {
        debug_assert!(order <= HUGE_ORDER);
        debug_assert!(start < self.frames());

        match order {
            HUGE_ORDER => self.get_huge(start).map(|f| (f, true)),
            _ => self.get_small(start, order),
        }
    }

    /// Free single frame, returning whether a whole huge page has become free.
    pub fn put(&self, frame: usize, order: u32) -> Result<bool, Error> {
        debug_assert!(order <= HUGE_ORDER);
        debug_assert!(frame < self.frames());

        if order == HUGE_ORDER {
            let i = (frame / Bitfield::LEN) % TREE_HUGE;
            let table = self.child(frame);

            if table[i]
                .compare_exchange(HugeEntry::new_huge(), HugeEntry::new_free(Bitfield::LEN))
                .is_err()
            {
                Err(Error::Address)
            } else {
                Ok(true)
            }
        } else {
            let i = (frame / Bitfield::LEN) % TREE_HUGE;
            let table = self.child(frame);

            let old = table[i].load();
            if old.huge() {
                self.partial_put_huge(old, frame, order)
            } else if old.free() <= Bitfield::LEN - (1 << order) {
                self.put_small(frame, order)
            } else {
                // log::error!("Addr p={frame:x} o={order} {old:?}");
                Err(Error::Address)
            }
        }
    }

    /// Returns if the frame is free. This might be racy!
    pub fn is_free(&self, frame: usize, order: u32) -> bool {
        debug_assert!(frame % (1 << order) == 0);
        if order > Bitfield::ORDER || frame + (1 << order) > self.frames() {
            return false;
        }

        let table = self.child(frame);
        let i = (frame / Bitfield::LEN) % TREE_HUGE;
        let entry = table[i].load();

        if entry.free() < (1 << order) {
            false
        } else if entry.free() == Bitfield::LEN {
            true
        } else {
            let bitfield = self.bitfield(frame);
            bitfield.is_zero(frame % Bitfield::LEN, order)
        }
    }

    /// Debug function, returning the number of allocated frames and performing internal checks.
    #[allow(unused)]
    pub fn free_frames(&self) -> usize {
        let mut free = 0;
        self.for_each_huge_frame(|_, f| free += f);
        free
    }

    #[allow(unused)]
    pub fn free_huge(&self) -> usize {
        let mut huge = 0;
        self.for_each_huge_frame(|_, f| huge += (f == HUGE_FRAMES) as usize);
        huge
    }

    /// Debug function returning number of free frames in each order 9 chunk
    pub fn for_each_huge_frame<F: FnMut(usize, usize)>(&self, mut f: F) {
        for (ti, table) in self.children.iter().enumerate() {
            for (ci, child) in table.iter().enumerate() {
                f(ti * TREE_HUGE + ci, child.load().free())
            }
        }
    }

    pub fn free_at(&self, frame: usize, order: u32) -> usize {
        match order {
            0 => self.is_free(frame, 0) as _,
            HUGE_ORDER => {
                let i = (frame / Bitfield::LEN) % TREE_HUGE;
                let child = self.child(frame)[i].load();
                child.free()
            }
            _ => 0,
        }
    }

    fn free_all(&self) {
        // Init tables
        let (last, tables) = unsafe { self.children.split_last().unwrap_unchecked() };
        // Table is fully included in the memory range
        for table in tables {
            table.atomic_fill(HugeEntry::new_free(Bitfield::LEN));
        }
        // Table is only partially included in the memory range
        for (i, entry) in last.iter().enumerate() {
            let frame = tables.len() * TREE_FRAMES + i * Bitfield::LEN;
            let free = self.frames().saturating_sub(frame).min(Bitfield::LEN);
            entry.store(HugeEntry::new_free(free));
        }

        // Init bitfields
        let last_i = self.frames() / Bitfield::LEN;
        let (included, mut remainder) = unsafe { self.bitfields.split_at_unchecked(last_i) };
        // Bitfield is fully included in the memory range
        for bitfield in included {
            bitfield.fill(false);
        }
        // Bitfield might be only partially included in the memory range
        if let Some((last, excluded)) = remainder.split_first() {
            let end = self.frames() - included.len() * Bitfield::LEN;
            debug_assert!(end <= Bitfield::LEN);
            last.set(0..end, false);
            last.set(end..Bitfield::LEN, true);
            remainder = excluded;
        }
        // Not part of the final memory range
        for bitfield in remainder {
            bitfield.fill(true);
        }
    }

    fn reserve_all(&self) {
        // Init table
        let (last, tables) = unsafe { self.children.split_last().unwrap_unchecked() };
        // Table is fully included in the memory range
        for table in tables {
            table.atomic_fill(HugeEntry::new_huge());
        }
        // Table is only partially included in the memory range
        let last_i = (self.frames() / Bitfield::LEN) - tables.len() * TREE_HUGE;
        let (included, remainder) = unsafe { last.split_at_unchecked(last_i) };
        for entry in included {
            entry.store(HugeEntry::new_huge());
        }
        // Remainder is allocated as small frames
        for entry in remainder {
            entry.store(HugeEntry::new_free(0));
        }

        // Init bitfields
        let last_i = self.frames() / Bitfield::LEN;
        let (included, remainder) = unsafe { self.bitfields.split_at_unchecked(last_i) };
        // Bitfield is fully included in the memory range
        for bitfield in included {
            bitfield.fill(false);
        }
        // Bitfield might be only partially included in the memory range
        for bitfield in remainder {
            bitfield.fill(true);
        }
    }

    /// Allocate frames up to order 8 (or up to order 10 for 16K)
    fn get_small(&self, start: usize, order: u32) -> Result<(usize, bool), Error> {
        debug_assert!(order < Bitfield::ORDER);

        let first_bf_i = align_down(start / Bitfield::LEN, TREE_HUGE);
        let start_bf_e = (start / Bitfield::ENTRY_BITS) % Bitfield::ENTRIES;
        let table = self.child(start);
        let offset = (start / Bitfield::LEN) % TREE_HUGE;

        for j in 0..TREE_HUGE {
            let i = (j + offset) % TREE_HUGE;

            if let Ok(child) = table[i].fetch_update(|v| v.dec(1 << order)) {
                let bf_i = first_bf_i + i;
                // start with the previous bitfield entry
                let bf_e = if j == 0 { start_bf_e } else { 0 };

                if let Ok(offset) =
                    unsafe { self.bitfields.get_unchecked(bf_i) }.set_first_zeros(bf_e, order)
                {
                    return Ok((bf_i * Bitfield::LEN + offset, child.free() == Bitfield::LEN));
                }

                // Revert counter
                table[i]
                    .fetch_update(|v| v.inc(Bitfield::LEN, 1 << order))
                    .map_err(|_| Error::UndoFailed)?;
            }
        }

        Err(Error::Memory)
    }

    /// Allocate huge frame
    fn get_huge(&self, start: usize) -> Result<usize, Error> {
        let table = self.child(start);
        let offset = (start / Bitfield::LEN) % TREE_HUGE;

        for i in 0..TREE_HUGE {
            let i = (offset + i) % TREE_HUGE;
            if table[i]
                .fetch_update(|v| v.mark_huge(Bitfield::LEN))
                .is_ok()
            {
                return Ok(align_down(start, TREE_FRAMES) + i * Bitfield::LEN);
            }
        }

        Err(Error::Memory)
    }

    fn put_small(&self, frame: usize, order: u32) -> Result<bool, Error> {
        debug_assert!(order < HUGE_ORDER);

        let bitfield = self.bitfield(frame);
        let i = frame % Bitfield::LEN;
        if bitfield.toggle(i, order, true).is_err() {
            return Err(Error::Address);
        }

        let table = self.child(frame);
        let i = (frame / Bitfield::LEN) % TREE_HUGE;
        match table[i].fetch_update(|v| v.inc(Bitfield::LEN, 1 << order)) {
            Err(_) => Err(Error::Retry),
            Ok(entry) => Ok(entry.free() + (1 << order) == Bitfield::LEN),
        }
    }

    fn partial_put_huge(&self, old: HugeEntry, frame: usize, order: u32) -> Result<bool, Error> {
        /// Retries the condition n times and returns if it was successful.
        /// This pauses the CPU between retries if possible.
        #[inline(always)]
        fn spin_wait(n: usize, mut cond: impl FnMut() -> bool) -> bool {
            for _ in 0..n {
                if cond() {
                    return true;
                }
                hint::spin_loop()
            }
            false
        }

        let i = (frame / Bitfield::LEN) % TREE_HUGE;
        let table = self.child(frame);
        let bitfield = self.bitfield(frame);

        // Try filling the whole bitfield
        if bitfield.toggle(0, Bitfield::ORDER, false).is_ok() {
            table[i]
                .compare_exchange(old, HugeEntry::default())
                .map_err(|_| Error::FailedPartialCase)?;
        }
        // Wait for parallel partial_put_huge to finish
        else if !spin_wait(RETRIES, || !table[i].load().huge()) {
            return Err(Error::ExceedReties);
        }

        self.put_small(frame, order)
    }
}

/// Manages huge frame, that can be allocated as base frames.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HugeEntry {
    /// Number of free 4K frames or u16::MAX for a huge frame.
    count: u16,
}

impl From<u16> for HugeEntry {
    fn from(count: u16) -> Self {
        HugeEntry { count }
    }
}

impl From<HugeEntry> for u16 {
    fn from(value: HugeEntry) -> Self {
        value.count
    }
}

impl Atomic for HugeEntry {
    type I = AtomicU16;
}

impl HugeEntry {
    /// Creates an entry marked as allocated huge frame.
    fn new_huge() -> Self {
        HugeEntry { count: u16::MAX }
    }

    /// Creates a new entry with the given free counter.
    fn new_free(free: usize) -> Self {
        HugeEntry { count: free as _ }
    }

    /// Returns wether this entry is allocated as huge frame.
    fn huge(self) -> bool {
        self.count == u16::MAX
    }

    /// Returns the free frames counter
    fn free(self) -> usize {
        if !self.huge() { self.count as _ } else { 0 }
    }

    /// Try to allocate this entry as huge frame.
    fn mark_huge(self, span: usize) -> Option<Self> {
        if self.free() == span {
            Some(Self::new_huge())
        } else {
            None
        }
    }

    /// Decrement the free frames counter.
    fn dec(self, num_frames: usize) -> Option<Self> {
        if !self.huge() && self.free() >= num_frames {
            Some(Self::new_free(self.free() - num_frames))
        } else {
            None
        }
    }

    /// Increments the free frames counter.
    fn inc(self, span: usize, num_frames: usize) -> Option<Self> {
        if !self.huge() && self.free() <= span - num_frames {
            Some(Self::new_free(self.free() + num_frames))
        } else {
            None
        }
    }
}
