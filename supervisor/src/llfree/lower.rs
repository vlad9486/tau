//! Lower allocator implementations

use core::num::{NonZero, NonZeroUsize};
use core::ops::Deref;
use core::sync::atomic::AtomicU32;
use core::slice;

use super::id::{RowId, TreeId, FrameId, HugeId};

use super::atomic::{Atom, Atomic};
use super::bitfield::Bitfield;
use super::util::{Align, size_of_slice, spin_wait};
use super::{
    Error, HUGE_FRAMES, HUGE_ORDER, MAX_FRAMES, MAX_ORDER, RETRIES, Stats, TREE_FRAMES, TREE_HUGE,
};

const _: () = assert!(Bitfield::LEN == HUGE_FRAMES);

pub struct Allocator {
    frames: NonZeroUsize,
    ptr: *mut u8,
    metadata: Metadata,
}

impl Allocator {
    /// Size in bytes needed to store the allocator metadata.
    ///
    /// Returns `None` when `frames` exceeds [`MAX_FRAMES`].
    pub const fn expected_size(frames: NonZeroUsize) -> Option<usize> {
        match Metadata::new(frames) {
            Some(metadata) => Some(metadata.size()),
            None => None,
        }
    }

    /// # Safety
    /// `ptr` must refer to a single readable, writable allocation of at least
    /// [`Self::expected_size`] bytes. All metadata bytes must be initialized
    /// (zeroed storage is sufficient), and the storage must remain valid until
    /// this allocator and every borrowed `Lower` have been dropped.
    ///
    /// During that time, access the storage only through this allocator. In
    /// particular, do not construct another allocator over overlapping storage
    /// or mutate or deallocate the backing buffer while this one is alive.
    /// Before allocation operations, call [`Self::reserve_all`] or
    /// [`Self::free_all`], unless the storage already holds valid metadata from
    /// a previous allocator with exactly the same frame count and layout.
    ///
    /// Null or misaligned pointers and unsupported sizes return `None`.
    pub unsafe fn new(frames: NonZeroUsize, ptr: *mut u8) -> Option<Self> {
        let metadata = Metadata::new(frames)?;
        if ptr.is_null()
            || !ptr.addr().is_multiple_of(align_of::<Align>())
            || ptr.addr().checked_add(metadata.size()).is_none()
        {
            return None;
        }

        Some(Allocator {
            frames,
            ptr,
            metadata,
        })
    }

    /// Reset all managed frames to free. The caller must have relinquished all
    /// outstanding frame allocations before using them again through this allocator.
    pub fn free_all(&mut self) {
        self.reset(true);
    }

    /// Reset all managed frames to reserved; subranges can then be freed with `put`.
    pub fn reserve_all(&mut self) {
        self.reset(false);
    }

    fn reset(&mut self, free: bool) {
        let m = self.metadata;
        // The constructor's contract keeps this storage valid and exclusive.
        // &mut self excludes every borrowed Lower during the reset.
        let bitfields = unsafe {
            slice::from_raw_parts_mut(self.ptr.cast::<Align<Bitfield>>(), m.bitfield_len)
        };
        let children = unsafe {
            slice::from_raw_parts_mut(
                self.ptr.add(m.bitfield_size).cast::<Align<Table>>(),
                m.table_len,
            )
        };

        // Initialize padded entries too, but never expose padding as free frames.
        for (table_i, table) in children.iter().enumerate() {
            for (entry_i, entry) in table.iter().enumerate() {
                let frame = table_i * TREE_FRAMES + entry_i * Bitfield::LEN;
                let count = self.frames.get().saturating_sub(frame).min(Bitfield::LEN);
                entry.store(if free {
                    HugeEntry::new_with(count)
                } else if count == Bitfield::LEN {
                    HugeEntry::new_huge()
                } else {
                    HugeEntry::new_with(0)
                });
            }
        }

        for (i, bitfield) in bitfields.iter().enumerate() {
            let count = (self.frames.get() - i * Bitfield::LEN).min(Bitfield::LEN);
            // Huge reservations use the counter sentinel and an empty bitmap.
            // Partial final huge frames reserve their padding in the bitmap.
            bitfield.fill(count != Bitfield::LEN);
            if free {
                bitfield.set(FrameId(0)..FrameId(count), false);
            }
        }
    }

    /// Consume the owner and hand off permanently allocated metadata.
    ///
    /// # Safety
    /// The backing storage must remain valid and writable for the rest of the
    /// program. After this handoff, access it only through the returned `Lower`;
    /// do not reset it or construct another allocator over the same storage.
    pub unsafe fn into_lower(self) -> Lower<'static> {
        // The caller guarantees permanent storage and exclusive handoff.
        unsafe { self.lower_with_lifetime() }
    }

    /// Borrow the metadata, preventing resets while the returned view is in use.
    pub fn as_lower(&self) -> Lower<'_> {
        // The constructor guarantees valid storage for this borrow.
        unsafe { self.lower_with_lifetime() }
    }

    /// # Safety
    /// The backing storage must remain valid for `'a`, with access restricted
    /// to this allocator and its shared views. No resets may occur during `'a`.
    unsafe fn lower_with_lifetime<'a>(&self) -> Lower<'a> {
        let m = self.metadata;
        let bitfields =
            unsafe { slice::from_raw_parts(self.ptr.cast::<Align<Bitfield>>(), m.bitfield_len) };
        let children = unsafe {
            slice::from_raw_parts(
                self.ptr.add(m.bitfield_size).cast::<Align<Table>>(),
                m.table_len,
            )
        };

        Lower {
            len: self.frames.get(),
            bitfields,
            children,
        }
    }
}

unsafe impl Send for Allocator {}

unsafe impl Sync for Allocator {}

/// Lower-level frame allocator.
///
/// This level implements the actual allocation/free operations.
/// Each allocation/free is limited to a chunk of [`MAX_ORDER`] frames.
///
/// Here the bitfields are [`HUGE_FRAMES`] bit large -> strong focus on huge frames.
/// Upon that is a table for each tree, with an entry per bitfield.
///
/// The parameter [`TREE_HUGE`] configures the number of table entries (huge frames per tree).
/// It has to be a multiple of 2!
///
/// Metadata is volatile and stored separately from the managed frames.
/// There is no crash recovery or persistent-metadata support.
#[derive(Debug)]
pub struct Lower<'a> {
    len: usize,
    bitfields: &'a [Align<Bitfield>],
    children: &'a [Align<Table>],
}

const _: () = assert!(TREE_HUGE < (1 << (u16::BITS as usize - HUGE_ORDER)));

/// Size of the dynamic metadata
#[derive(Clone, Copy)]
struct Metadata {
    bitfield_len: usize,
    bitfield_size: usize,
    table_len: usize,
    size: usize,
}

impl Metadata {
    const fn new(frames: NonZero<usize>) -> Option<Self> {
        let frames = frames.get();
        if frames > MAX_FRAMES {
            return None;
        }
        let bitfield_len = frames.div_ceil(Bitfield::LEN);
        let table_len = frames.div_ceil(TREE_FRAMES);
        // These sizes also respect cache-line alignment.
        let bitfield_size = match size_of_slice::<Bitfield>(bitfield_len) {
            Some(size) => size,
            None => return None,
        };
        let table_size = match size_of_slice::<Align<Table>>(table_len) {
            Some(size) => size,
            None => return None,
        };
        let size = match bitfield_size.checked_add(table_size) {
            Some(size) if size <= isize::MAX as usize => size,
            _ => return None,
        };
        Some(Self {
            bitfield_len,
            bitfield_size,
            table_len,
            size,
        })
    }

    const fn size(&self) -> usize {
        self.size
    }
}

impl<'a> Lower<'a> {
    fn allocation_size(order: usize) -> Result<usize, Error> {
        if order > MAX_ORDER {
            return Err(Error::OrderNotSuported);
        }
        1usize
            .checked_shl(order as u32)
            .ok_or(Error::OrderNotSuported)
    }

    fn validate_range(&self, frame: FrameId, order: usize) -> Result<usize, Error> {
        let size = Self::allocation_size(order)?;
        if !frame.is_aligned(order)
            || frame
                .0
                .checked_add(size)
                .is_none_or(|end| end > self.frames())
        {
            Err(Error::Address)
        } else {
            Ok(size)
        }
    }

    fn children(&self, tree: TreeId) -> Result<&Table, Error> {
        self.children
            .get(tree.0)
            .map(Deref::deref)
            .ok_or(Error::InvalidArgument)
    }

    fn bitfield(&self, huge: HugeId) -> Result<&Bitfield, Error> {
        self.bitfields
            .get(huge.0)
            .map(Deref::deref)
            .ok_or(Error::InvalidArgument)
    }

    pub fn frames(&self) -> usize {
        self.len
    }

    /// Allocate an aligned block of `1 << order` frames, searching all trees.
    ///
    /// Starts at `core_id % tree_count` and wraps around, without per-CPU
    /// reservations or additional metadata. Under concurrent updates, `Error::Memory` does
    /// not guarantee that the allocator is globally exhausted.
    pub fn alloc(&self, core_id: usize, order: usize) -> Result<FrameId, Error> {
        Self::allocation_size(order)?;
        let count = self.children.len();
        let first = core_id.checked_rem(count).ok_or(Error::Memory)?;
        for tree in (first..count).chain(0..first) {
            let start = TreeId(tree).as_row().ok_or(Error::InvalidArgument)?;
            match self.get(start, order) {
                Err(Error::Memory) => {}
                result => return result,
            }
        }
        Err(Error::Memory)
    }

    /// Try allocating a new `frame` in the [`TREE_FRAMES`] sized chunk at `start`.
    ///
    /// Returns the allocated frame.
    pub fn get(&self, start: RowId, order: usize) -> Result<FrameId, Error> {
        let size = Self::allocation_size(order)?;
        if start.0 >= self.frames().div_ceil(super::BITFIELD_ROW) {
            return Err(Error::InvalidArgument);
        }

        let tree = start.as_tree();
        let tree_start = tree.as_frame().ok_or(Error::InvalidArgument)?;
        let child_off = start.as_huge().child_idx();
        let children = self.children(tree)?;

        if order == MAX_ORDER {
            let table_pair = self.child_pairs(tree)?;
            for i in 0..TREE_HUGE / 2 {
                let i = (child_off / 2 + i) % (TREE_HUGE / 2);
                let table_pair = table_pair.get(i).ok_or(Error::InvalidArgument)?;
                if let Ok(_) = table_pair.fetch_update(|v| v.map(|v| v.mark_huge())) {
                    return Ok(FrameId(tree_start.0 + 2 * i * HUGE_FRAMES));
                }
            }
        } else if order == HUGE_ORDER {
            for i in 0..TREE_HUGE {
                let i = (child_off + i) % TREE_HUGE;
                let child = children.get(i).ok_or(Error::InvalidArgument)?;
                if let Ok(_) = child.fetch_update(|v| v.mark_huge()) {
                    return Ok(FrameId(tree_start.0 + i * HUGE_FRAMES));
                }
            }
        } else if order <= Bitfield::ORDER {
            let first_child = tree_start.as_huge();

            for j in 0..TREE_HUGE {
                let i = (child_off + j) % TREE_HUGE;
                let child = children.get(i).ok_or(Error::InvalidArgument)?;
                if let Ok(_) = child.fetch_update(|v| v.dec(size)) {
                    let bf_i = HugeId(first_child.0 + i);
                    // start with the bitfield row from the last allocation
                    if let Ok(offset) = self.bitfield(bf_i)?.set_first_zeros(start, order) {
                        let base = bf_i.as_frame().ok_or(Error::InvalidArgument)?;
                        return Ok(FrameId(base.0 + offset.0));
                    }
                    if child.fetch_update(|v| v.inc(size)).is_err() {
                        return Err(Error::UndoFailed);
                    }
                }
            }
        } else {
            return Err(Error::OrderNotSuported);
        }
        // log::debug!("Nothing found o={order}");
        Err(Error::Memory)
    }

    /// Try allocating a specific `frame`.
    pub fn get_at(&self, frame: FrameId, order: usize) -> Result<(), Error> {
        let size = self.validate_range(frame, order)?;

        let i = (frame.as_huge().0) % TREE_HUGE;
        let children = self
            .children(frame.as_tree())?
            .get(i)
            .ok_or(Error::InvalidArgument)?;

        if order == MAX_ORDER {
            if let Ok(_) = self
                .child_pairs(frame.as_tree())?
                .get(i / 2)
                .ok_or(Error::InvalidArgument)?
                .fetch_update(|v| v.map(|v| v.mark_huge()))
            {
                return Ok(());
            }
        } else if order == HUGE_ORDER {
            if let Ok(_) = children.fetch_update(|v| v.mark_huge()) {
                return Ok(());
            }
        } else if order <= Bitfield::ORDER {
            if let Ok(_) = children.fetch_update(|v| v.dec(size)) {
                if let Ok(()) = self.bitfield(frame.as_huge())?.toggle(frame, order, false) {
                    return Ok(());
                }
                // Undo decrement
                if children.fetch_update(|v| v.inc(size)).is_err() {
                    return Err(Error::UndoFailed);
                }
            }
        } else {
            return Err(Error::OrderNotSuported);
        }
        Err(Error::Address)
    }

    /// Free an allocated range, including a subrange of a huge allocation.
    ///
    /// The caller must own the entire range and relinquish it on success.
    /// Concurrent or repeated frees of overlapping ranges are invalid; errors
    /// are best-effort diagnostics, not a substitute for tracking ownership.
    pub fn put(&self, _core_id: usize, frame: FrameId, order: usize) -> Result<(), Error> {
        let size = self.validate_range(frame, order)?;

        let i = frame.as_huge().child_idx();
        let children = self.children(frame.as_tree())?;

        if order == MAX_ORDER {
            let table_pair = self.child_pairs(frame.as_tree())?;
            let table_pair = table_pair.get(i / 2).ok_or(Error::InvalidArgument)?;
            if let Err(_old) = table_pair.compare_exchange(
                HugePair(HugeEntry::new_huge(), HugeEntry::new_huge()),
                HugePair(
                    HugeEntry::new_with(Bitfield::LEN),
                    HugeEntry::new_with(Bitfield::LEN),
                ),
            ) {
                // log::error!("Addr {frame:?} o={order} {old:?}");
                Err(Error::Address)
            } else {
                Ok(())
            }
        } else if order == HUGE_ORDER {
            let child = children.get(i).ok_or(Error::InvalidArgument)?;
            if let Err(_old) =
                child.compare_exchange(HugeEntry::new_huge(), HugeEntry::new_with(Bitfield::LEN))
            {
                // log::error!("Addr {frame:?} o={order} {old:?}");
                Err(Error::Address)
            } else {
                Ok(())
            }
        } else if order <= Bitfield::ORDER {
            let child = children.get(i).ok_or(Error::InvalidArgument)?;
            let old = child.load();
            if old.huge() {
                self.partial_put_huge(old, frame, order)
            } else if old.free() <= Bitfield::LEN - size {
                self.put_small(frame, order)
            } else {
                // log::error!("Addr {frame:?} o={order} {old:?}");
                Err(Error::Address)
            }
        } else {
            Err(Error::OrderNotSuported)
        }
    }

    /// Returns if the frame is free. This might be racy!
    pub fn is_free(&self, frame: FrameId, order: usize) -> bool {
        const TREE_ORDER: usize = TREE_FRAMES.ilog2() as usize;
        let Some(size) = 1usize.checked_shl(order as u32) else {
            return false;
        };
        if order > TREE_ORDER
            || !frame.is_aligned(order)
            || frame
                .0
                .checked_add(size)
                .is_none_or(|end| end > self.frames())
        {
            return false;
        }

        let i = frame.as_huge().child_idx();
        let Ok(children) = self.children(frame.as_tree()) else {
            return false;
        };

        if size == TREE_FRAMES {
            children.iter().all(|e| e.load().free() == Bitfield::LEN)
        } else if order == MAX_ORDER {
            // multiple huge frames
            let Ok(pairs) = self.child_pairs(frame.as_tree()) else {
                return false;
            };
            pairs
                .get(i / 2)
                .is_some_and(|pair| pair.load().all(|e| e.free() == Bitfield::LEN))
        } else if order == HUGE_ORDER {
            children
                .get(i)
                .is_some_and(|child| child.load().free() == Bitfield::LEN)
        } else if order <= Bitfield::ORDER {
            let Some(child) = children.get(i).map(|child| child.load()) else {
                return false;
            };
            if child.free() < size {
                false
            } else if child.free() == Bitfield::LEN {
                true
            } else {
                self.bitfield(frame.as_huge())
                    .is_ok_and(|bf| bf.is_zero(frame, order))
            }
        } else {
            false
        }
    }

    /// Returns statistics.
    pub fn stats(&self) -> Stats {
        let mut stats = Stats::default();
        for children in self.children {
            let mut free = 0usize;
            for child in children.iter() {
                let f = child.load().free();
                stats.free_frames += f;
                stats.free_huge += (f == HUGE_FRAMES) as usize;
                free += f;
            }
            stats.free_trees += (free == TREE_FRAMES) as usize;
        }
        stats
    }

    /// Returns statistics at a specific frame, huge frame, or tree.
    pub fn stats_at(&self, frame: FrameId, order: usize) -> Result<Stats, Error> {
        const TREE_ORDER: usize = TREE_FRAMES.ilog2() as usize;
        if frame.0 >= self.frames() {
            return Err(Error::Address);
        }
        let children = self.children(frame.as_tree())?;
        let i = frame.as_huge();
        match order {
            0 => Ok(Stats {
                free_frames: (children
                    .get(i.child_idx())
                    .ok_or(Error::InvalidArgument)?
                    .load()
                    .free()
                    > 0
                    && self.bitfield(i)?.is_zero(frame, 0)) as usize,
                free_huge: 0,
                free_trees: 0,
            }),
            HUGE_ORDER => {
                let free = children
                    .get(i.child_idx())
                    .ok_or(Error::InvalidArgument)?
                    .load()
                    .free();
                Ok(Stats {
                    free_frames: free,
                    free_huge: free / HUGE_FRAMES,
                    free_trees: 0,
                })
            }
            TREE_ORDER => {
                let mut stats = children.iter().fold(Stats::default(), |mut acc, e| {
                    let f = e.load().free();
                    acc.free_frames += f;
                    acc.free_huge += f / HUGE_FRAMES;
                    acc
                });
                stats.free_trees = stats.free_frames / TREE_FRAMES;
                Ok(stats)
            }
            _ => Ok(Stats::default()),
        }
    }

    /// Returns the table with pair entries that can be updated at once.
    fn child_pairs(&self, tree: TreeId) -> Result<&[Atom<HugePair>; TREE_HUGE / 2], Error> {
        Ok(&self.children(tree)?.pairs)
    }

    fn put_small(&self, frame: FrameId, order: usize) -> Result<(), Error> {
        if order >= HUGE_ORDER {
            return Err(Error::OrderNotSuported);
        }

        let bitfield = self.bitfield(frame.as_huge())?;
        if bitfield.toggle(frame, order, true).is_err() {
            // log::error!(
            //     "L1 put failed o={order} i={} p={frame:?}",
            //     frame.0 % Bitfield::LEN
            // );
            return Err(Error::Address);
        }

        let children = self.children(frame.as_tree())?;
        let i = frame.as_huge().child_idx();
        let child = children.get(i).ok_or(Error::InvalidArgument)?;
        let size = Self::allocation_size(order)?;
        match child.fetch_update(|v| v.inc(size)) {
            Ok(_) => Ok(()),
            Err(_entry) => Err(Error::FailedToIncrement),
        }
    }

    fn partial_put_huge(&self, old: HugeEntry, frame: FrameId, order: usize) -> Result<(), Error> {
        // log::info!("partial free of huge frame {frame:?} o={order}");
        let i = frame.as_huge().child_idx();
        let children = self.children(frame.as_tree())?;
        let bitfield = self.bitfield(frame.as_huge())?;
        let child = children.get(i).ok_or(Error::InvalidArgument)?;
        // Try filling the whole bitfield
        if bitfield.toggle(FrameId(0), Bitfield::ORDER, false).is_ok() {
            // TODO:
            let _ = child.compare_exchange(old, HugeEntry::new());
        }
        // Wait for parallel partial_put_huge to finish
        else if !spin_wait(RETRIES, || !child.load().huge()) {
            return Err(Error::Retry);
        }

        self.put_small(frame, order)
    }
}

/// Every access, including a single counter update, uses the same u32 atomic.
#[derive(Debug)]
struct Table {
    pairs: [Atom<HugePair>; TREE_HUGE / 2],
}

impl Table {
    fn get(&self, index: usize) -> Option<Child<'_>> {
        self.pairs.get(index / 2).map(|pair| Child {
            pair,
            second: !index.is_multiple_of(2),
        })
    }

    fn iter(&self) -> impl Iterator<Item = Child<'_>> {
        self.pairs.iter().flat_map(|pair| {
            [
                Child {
                    pair,
                    second: false,
                },
                Child { pair, second: true },
            ]
        })
    }
}

/// A counter view; it never creates a smaller atomic reference.
struct Child<'a> {
    pair: &'a Atom<HugePair>,
    second: bool,
}

impl Child<'_> {
    fn entry(&self, pair: HugePair) -> HugeEntry {
        if self.second { pair.1 } else { pair.0 }
    }

    fn load(&self) -> HugeEntry {
        self.entry(self.pair.load())
    }

    fn store(&self, value: HugeEntry) {
        let _ = self.fetch_update(|_| Some(value));
    }

    fn fetch_update(
        &self,
        mut f: impl FnMut(HugeEntry) -> Option<HugeEntry>,
    ) -> Result<HugeEntry, HugeEntry> {
        self.pair
            .fetch_update(|pair| {
                let entry = f(self.entry(pair))?;
                Some(if self.second {
                    HugePair(pair.0, entry)
                } else {
                    HugePair(entry, pair.1)
                })
            })
            .map(|pair| self.entry(pair))
            .map_err(|pair| self.entry(pair))
    }

    fn compare_exchange(&self, current: HugeEntry, new: HugeEntry) -> Result<HugeEntry, HugeEntry> {
        self.fetch_update(|entry| (entry == current).then_some(new))
    }
}

/// Manages huge frame, that can be allocated as base frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HugeEntry(u16);

impl HugeEntry {
    fn new() -> Self {
        Self(0)
    }

    /// Creates an entry marked as allocated huge frame.
    fn new_huge() -> Self {
        Self(u16::MAX)
    }

    /// Creates a new entry with the given free counter.
    fn new_with(free: usize) -> Self {
        Self(free as u16)
    }

    /// Returns wether this entry is allocated as huge frame.
    fn huge(self) -> bool {
        self.0 == u16::MAX
    }

    /// Returns the free frames counter
    fn free(self) -> usize {
        if self.huge() { 0 } else { self.0 as usize }
    }

    /// Try to allocate this entry as huge frame.
    fn mark_huge(self) -> Option<Self> {
        if self.free() == Bitfield::LEN {
            Some(Self::new_huge())
        } else {
            None
        }
    }

    /// Decrement the free frames counter.
    fn dec(self, num_frames: usize) -> Option<Self> {
        if !self.huge() && self.free() >= num_frames {
            Some(Self::new_with(self.free() - num_frames))
        } else {
            None
        }
    }

    /// Increments the free frames counter.
    fn inc(self, num_frames: usize) -> Option<Self> {
        if !self.huge() && self.free() <= Bitfield::LEN.checked_sub(num_frames)? {
            Some(Self::new_with(self.free() + num_frames))
        } else {
            None
        }
    }
}

/// Pair of huge entries that can be changed at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HugePair(HugeEntry, HugeEntry);

impl Atomic for HugePair {
    type I = AtomicU32;
}

impl HugePair {
    /// Apply `f` to both entries.
    fn map(self, f: impl Fn(HugeEntry) -> Option<HugeEntry>) -> Option<HugePair> {
        Some(HugePair(f(self.0)?, f(self.1)?))
    }
    /// Check if `f` is true for both entries.
    fn all(self, f: impl Fn(HugeEntry) -> bool) -> bool {
        f(self.0) && f(self.1)
    }
}

impl From<u32> for HugePair {
    fn from(value: u32) -> Self {
        Self(HugeEntry(value as u16), HugeEntry((value >> 16) as u16))
    }
}

impl From<HugePair> for u32 {
    fn from(value: HugePair) -> Self {
        u32::from(value.0.0) | (u32::from(value.1.0) << 16)
    }
}
