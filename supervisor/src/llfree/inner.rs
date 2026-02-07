//! Upper allocator implementation

use core::cmp::Ordering;
use core::mem::MaybeUninit;
use core::ops::Range;
use core::{fmt, slice};

use super::atomic::Atom;
use super::local::{Local, LocalTree};
use super::lower::{HugeEntry, Lower, Bitfield};
use super::trees::{Kind, Tree, Trees};
use super::util::Align;
use super::{Error, Flags, Init, HUGE_ORDER, RETRIES, TREE_FRAMES, TREE_HUGE};

/// This allocator splits its memory range into chunks.
/// These chunks are reserved by CPUs to reduce sharing.
/// Allocations/frees within the chunk are handed over to the
/// lower allocator.
/// These chunks are, due to the inner workers of the lower allocator,
/// called *trees*.
///
/// This allocator stores the tree entries in a packed array.
/// For reservations, the allocator simply scans the array for free entries,
/// while prioritizing partially empty already fragmented chunks to avoid
/// further fragmentation.
///
/// This volatile shared metadata is rebuild on boot from
/// the persistent metadata of the lower allocator.
pub struct LLFree<'a> {
    /// CPU local data
    ///
    /// Other CPUs can access this if they drain cores.
    local: &'a [Align<Local>],
    /// Metadata of the lower alloc
    lower: Lower<'a>,
    /// Manages the allocators trees
    trees: Trees<'a>,
}

struct Metadata<'a> {
    frames: usize,
    local: &'a [Align<Local>],
    tree_entries: &'a [Atom<Tree>],
    children: &'a mut [Align<[Atom<HugeEntry>; TREE_HUGE]>],
    bitfields: &'a [Align<Bitfield>],
}

impl<'a> LLFree<'a> {
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    fn create_metadata<'b>(
        cores: usize,
        frames: usize,
        init: bool,
        ptr: *mut MaybeUninit<[usize; 0o1000]>,
    ) -> (usize, Metadata<'b>) {
        const LOCAL_SIZE: usize =
            size_of::<Align<Local>>().next_multiple_of(align_of::<Align<Local>>());

        const fn tree_size(frames: usize) -> usize {
            type T = Atom<Tree>;

            tree_len(frames) * size_of::<T>()
        }

        const fn tree_len(frames: usize) -> usize {
            frames.div_ceil(TREE_FRAMES)
        }

        const fn children_size(frames: usize) -> usize {
            type T = Align<[Atom<HugeEntry>; TREE_HUGE]>;

            children_len(frames) * size_of::<T>().next_multiple_of(align_of::<T>())
        }

        const fn children_len(frames: usize) -> usize {
            frames.div_ceil(TREE_FRAMES)
        }

        const fn bitfields_size(frames: usize) -> usize {
            type T = Align<Bitfield>;

            bitfields_len(frames) * size_of::<T>().next_multiple_of(align_of::<T>())
        }

        const fn bitfields_len(frames: usize) -> usize {
            frames.div_ceil(Bitfield::LEN)
        }

        let p0 = (cores * LOCAL_SIZE).div_ceil(0x1000);
        let p1 = tree_size(frames).div_ceil(0x1000);
        let p2 = children_size(frames).div_ceil(0x1000);
        let p3 = bitfields_size(frames).div_ceil(0x1000);

        if init {
            let metadata_array = unsafe { slice::from_raw_parts_mut(ptr, p0 + p1 + p2 + p3) };
            for page in metadata_array {
                page.write([0; 512]);
            }
        }

        unsafe {
            let local = slice::from_raw_parts(ptr.cast(), cores);
            let tree_entries = slice::from_raw_parts(ptr.add(p0).cast(), tree_len(frames));
            let children = slice::from_raw_parts_mut(ptr.add(p0 + p1).cast(), children_len(frames));
            let bitfields =
                slice::from_raw_parts(ptr.add(p0 + p1 + p2).cast(), bitfields_len(frames));

            let metadata = Metadata {
                frames,
                local,
                tree_entries,
                children,
                bitfields,
            };
            (p0 + p1 + p2 + p3, metadata)
        }
    }

    /// Initialize the allocator.
    #[cold]
    pub fn new(
        init: Init,
        cores: usize,
        frames: usize,
        ptr: *mut MaybeUninit<[usize; 0o1000]>,
    ) -> Result<(usize, Self), Error> {
        let (
            pages,
            Metadata {
                frames,
                local,
                tree_entries,
                children,
                bitfields,
            },
        ) = Self::create_metadata(cores, frames, !matches!(&init, Init::None), ptr);
        let cores = local.len();

        if frames < TREE_FRAMES * cores {
            return Err(Error::Initialization);
        }

        // Create lower allocator
        let lower = Lower::new(frames, init, bitfields, children)?;

        // Init tree array
        let tree_init = (init != Init::None).then_some(|start| lower.free_in_tree(start));
        let trees = Trees::new(tree_entries, tree_init);

        Ok((
            pages,
            LLFree {
                local,
                lower,
                trees,
            },
        ))
    }

    pub fn get(&self, core: usize, flags: Flags) -> Result<usize, Error> {
        if flags.order > HUGE_ORDER {
            return Err(Error::Memory);
        }
        // We might have more cores than cpu-local data
        let core = unsafe { core.checked_rem(self.local.len()).unwrap_unchecked() };

        let mut old = LocalTree::none();
        // Try local reservation first (if enough memory)
        if self.trees.len() > 3 * self.cores() {
            // Retry allocation up to n times if it fails due to a concurrent update
            for _ in 0..RETRIES {
                match self.get_from_local(core, flags.into(), flags.order) {
                    Ok(frame) => return Ok(frame),
                    Err((Error::Retry, _)) => {}
                    Err((Error::Memory, old_n)) => {
                        old = old_n;
                        break;
                    }
                    Err((e, _)) => return Err(e),
                }
            }

            match self.reserve_and_get(core, flags, old) {
                Err(Error::Memory) => {}
                r => return r,
            }

            for _ in 0..RETRIES {
                match self.steal_from_reserved(core, flags) {
                    Err(Error::Retry) => {}
                    Err(Error::Memory) => break,
                    r => return r,
                }
            }
        }
        // Fallback to global allocation (ignoring local reservations)
        let start = if old.present() { old.frame() } else { 0 };
        self.get_any_global(start / TREE_FRAMES, flags)
    }

    pub fn put(&self, core: usize, frame: usize, flags: Flags) -> Result<(), Error> {
        if frame >= self.lower.frames() {
            return Err(Error::Memory);
        }
        let order = flags.order;

        // First free the frame in the lower allocator
        self.lower.put(frame, order)?;

        // Then update local / global counters
        let i = frame / TREE_FRAMES;
        let local = unsafe { self.local.get_unchecked(core) };
        // Update the put-reserve heuristic
        let may_reserve = self.cores() > 1 && local.frees_push(i);

        // Try update own trees first
        let num_frames = 1usize << order;
        if order >= HUGE_ORDER {
            let preferred = local.preferred(Kind::Huge);
            if preferred.fetch_update(|v| v.inc(frame, 1 << order)).is_ok() {
                return Ok(());
            }
        } else {
            // Might be movable or fixed
            for kind in [Kind::Movable, Kind::Fixed] {
                let preferred = local.preferred(kind);
                if preferred.fetch_update(|v| v.inc(frame, 1 << order)).is_ok() {
                    return Ok(());
                }
            }
        }

        // Increment or reserve globally
        if let Some(tree) = self.trees.inc_or_reserve(i, num_frames, may_reserve) {
            // Change preferred tree to speedup future frees
            let entry = LocalTree::with(i * TREE_FRAMES, tree.free() + num_frames);
            let flags = Flags {
                order,
                movable: tree.kind() == Kind::Movable,
            };
            let kind = flags.into();
            self.swap_reserved(local.preferred(kind), entry, kind);
        }
        Ok(())
    }

    pub fn is_free(&self, frame: usize, order: u32) -> bool {
        if frame < self.lower.frames() {
            self.lower.is_free(frame, order)
        } else {
            false
        }
    }

    pub fn frames(&self) -> usize {
        self.lower.frames()
    }

    pub fn cores(&self) -> usize {
        self.local.len()
    }

    pub fn allocated_frames(&self) -> usize {
        self.frames() - self.free_frames()
    }

    pub fn drain(&self, core: usize) -> Result<(), Error> {
        for kind in [Kind::Fixed, Kind::Movable, Kind::Huge] {
            let core = unsafe { core.checked_rem(self.local.len()).unwrap_unchecked() };
            let preferred = unsafe { self.local.get_unchecked(core) }.preferred(kind);
            self.swap_reserved(preferred, LocalTree::none(), kind);
        }
        Ok(())
    }

    pub fn free_frames(&self) -> usize {
        // Global array
        let mut frames = self.trees.free_frames();
        // Frames allocated in reserved trees
        for local in self.local.iter() {
            for kind in [Kind::Fixed, Kind::Movable, Kind::Huge] {
                let preferred = local.preferred(kind).load();
                if preferred.present() {
                    frames += preferred.free();
                }
            }
        }
        frames
    }

    pub fn free_huge(&self) -> usize {
        self.lower.free_huge()
    }

    pub fn free_at(&self, frame: usize, order: u32) -> usize {
        match order.cmp(&HUGE_ORDER) {
            Ordering::Equal => {
                let global = self.trees.get(frame / TREE_FRAMES);
                if global.reserved() {
                    for local in self.local {
                        for kind in [Kind::Fixed, Kind::Movable, Kind::Huge] {
                            let preferred = local.preferred(kind).load();
                            if preferred.present()
                                && preferred.frame() / TREE_FRAMES == frame / TREE_FRAMES
                            {
                                return global.free() + preferred.free();
                            }
                        }
                    }
                }
                global.free()
            }
            Ordering::Less => self.lower.free_at(frame, order),
            Ordering::Greater => 0,
        }
    }

    pub fn validate(&self) {
        debug_assert_eq!(self.free_frames(), self.lower.free_frames());
        debug_assert_eq!(self.free_huge(), self.lower.free_huge());
        let mut reserved = 0;
        for (i, tree) in self.trees.entries.iter().enumerate() {
            let tree = tree.load();
            if !tree.reserved() {
                let free = self.lower.free_in_tree(i * TREE_FRAMES);
                debug_assert_eq!(tree.free(), free);
            } else {
                reserved += 1;
            }
        }
        for local in self.local {
            for kind in [Kind::Movable, Kind::Fixed, Kind::Huge] {
                let tree = local.preferred(kind).load();
                if tree.present() {
                    let global = self.trees.get(tree.frame() / TREE_FRAMES);
                    let free = self.lower.free_in_tree(tree.frame());
                    debug_assert_eq!(tree.free() + global.free(), free);
                    reserved -= 1;
                }
            }
        }
        debug_assert!(reserved == 0);
    }
}

impl LLFree<'_> {
    fn lower_get(&self, mut tree: LocalTree, order: u32) -> Result<LocalTree, Error> {
        let (frame, _huge) = self.lower.get(tree.frame(), order)?;
        unsafe { tree.set_frame_checked(frame).unwrap_unchecked() };
        unsafe {
            tree.set_free_checked(tree.free() - (1 << order))
                .unwrap_unchecked()
        };
        Ok(tree)
    }

    fn get_from_local(
        &self,
        core: usize,
        kind: Kind,
        order: u32,
    ) -> Result<usize, (Error, LocalTree)> {
        let preferred = unsafe { self.local.get_unchecked(core) }.preferred(kind);

        match preferred.fetch_update(|v| v.dec(1 << order)) {
            Ok(old) => match self.lower_get(old, order) {
                Ok(new) => {
                    if old.frame() / 64 != new.frame() / 64 {
                        let _ = preferred.fetch_update(|v| v.set_start(new.frame(), false));
                    }
                    Ok(new.frame())
                }
                Err(e) => {
                    unsafe {
                        let _ = self
                            .trees
                            .entries
                            .get_unchecked(old.frame() / TREE_FRAMES)
                            .fetch_update(|v| Some(v.inc(1 << order)));
                    }
                    Err((e, old))
                }
            },
            Err(old) => {
                if old.present() && self.sync_with_global(preferred, order, old) {
                    return Err((Error::Retry, old));
                }
                Err((Error::Memory, old))
            }
        }
    }

    /// Frees from other CPUs update the global entry -> sync free counters.
    ///
    /// Returns if the global counter was large enough
    fn sync_with_global(&self, preferred: &Atom<LocalTree>, order: u32, old: LocalTree) -> bool {
        if !old.present() || old.free() >= 1 << order {
            return false;
        }

        let i = old.frame() / TREE_FRAMES;
        let min = (1usize << order).saturating_sub(old.free());

        if let Some(global) = self.trees.sync(i, min) {
            let new = LocalTree::with(old.frame(), old.free() + global.free());
            if preferred.compare_exchange(old, new).is_ok() {
                return true;
            }
            let _ = unsafe { self.trees.entries.get_unchecked(i) }
                .fetch_update(|v| Some(v.inc(global.free())));
            false
        } else {
            false
        }
    }

    /// Reserve a new tree and allocate the frame in it
    fn reserve_and_get(&self, core: usize, flags: Flags, old: LocalTree) -> Result<usize, Error> {
        // Try reserve new tree
        let preferred = self.local[core].preferred(flags.into());
        let start = if old.present() {
            old.frame() / TREE_FRAMES
        } else {
            // Different initial starting point for every core
            self.trees.len() / self.local.len() * core
        };
        const CL: usize = align_of::<Align>() / size_of::<Tree>();
        let near = ((self.trees.len() / self.cores()) / 4).clamp(CL / 4, CL * 2);

        let reserve = |i: usize, range: Range<usize>| {
            let range = (1 << flags.order).max(range.start)..range.end;
            if let Ok(old) = unsafe { self.trees.entries.get_unchecked(i) }
                .fetch_update(|v| v.reserve(range.clone(), flags.into()))
            {
                match self.lower_get(LocalTree::with(i * TREE_FRAMES, old.free()), flags.order) {
                    Ok(new) => {
                        self.swap_reserved(preferred, new, flags.into());
                        Ok(new.frame())
                    }
                    Err(e) => {
                        self.trees.unreserve(i, old.free(), flags.into());
                        Err(e)
                    }
                }
            } else {
                Err(Error::Memory)
            }
        };

        // Over half filled trees
        let range = (TREE_FRAMES / 16)..(TREE_FRAMES / 2);
        match self
            .trees
            .search(start, 1, near, |i| reserve(i, range.clone()))
        {
            Err(Error::Memory) => {}
            r => return r,
        }
        // Partially filled
        let range = (TREE_FRAMES / 64)..(TREE_FRAMES - TREE_FRAMES / 16);
        match self
            .trees
            .search(start, 1, near, |i| reserve(i, range.clone()))
        {
            Err(Error::Memory) => {}
            r => return r,
        }
        // Not free
        let range = 0..TREE_FRAMES;
        match self
            .trees
            .search(start, 1, near, |i| reserve(i, range.clone()))
        {
            Err(Error::Memory) => {}
            r => return r,
        }
        // Any
        let range = 0..usize::MAX;
        self.trees
            .search(start, 1, near, |i| reserve(i, range.clone()))
    }

    /// Steal a tree from another core
    fn steal_from_reserved(&self, core: usize, flags: Flags) -> Result<usize, Error> {
        let kind = Kind::from(flags);
        for i in 1..self.local.len() {
            let target_core =
                unsafe { (core + i).checked_rem(self.local.len()).unwrap_unchecked() };

            for o_kind in 0..Kind::LEN {
                let t_kind = Kind::from((kind as usize + o_kind) % Kind::LEN);

                if t_kind.accepts(kind) {
                    // Less strict kind, just allocate
                    match self.get_from_local(target_core, t_kind, flags.order) {
                        Err((Error::Memory, _)) => {}
                        r => return r.map_err(|e| e.0),
                    }
                } else {
                    // More strict kind, steal and convert tree
                    match self.steal_tree(target_core, t_kind, flags.order) {
                        Ok(stolen) => {
                            self.swap_reserved(self.local[core].preferred(kind), stolen, kind);
                            return Ok(stolen.frame());
                        }
                        Err(Error::Memory) => {}
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        Err(Error::Memory)
    }

    fn steal_tree(&self, core: usize, kind: Kind, order: u32) -> Result<LocalTree, Error> {
        let preferred = self.local[core].preferred(kind);
        match preferred.fetch_update(|v| v.steal(1 << order)) {
            Ok(stolen) => match self.lower_get(stolen, order) {
                Ok(stolen) => Ok(stolen),
                Err(e) => {
                    debug_assert!(stolen.present());
                    let i = stolen.frame() / TREE_FRAMES;
                    self.trees.unreserve(i, stolen.free(), kind);
                    Err(e)
                }
            },
            _ => Err(Error::Memory),
        }
    }

    fn get_any_global(&self, start_idx: usize, flags: Flags) -> Result<usize, Error> {
        self.trees.search(start_idx, 0, self.trees.len(), |i| {
            let tree = unsafe { self.trees.entries.get_unchecked(i) };
            let old = tree
                .fetch_update(|v| v.dec_force(1 << flags.order, flags.into()))
                .map_err(|_| Error::Memory)?;

            match self.lower.get(i * TREE_FRAMES, flags.order) {
                Ok((frame, _)) => Ok(frame),
                Err(e) => {
                    let exp = old.dec_force(1 << flags.order, flags.into()).ok_or(e)?;
                    if tree.compare_exchange(exp, old).is_err() {
                        tree.fetch_update(|v| Some(v.inc(1 << flags.order)))
                            .map_err(|_| Error::UndoFailed)?;
                    }
                    Err(e)
                }
            }
        })
    }

    /// Swap the current reserved tree out replacing it with a new one.
    /// The old tree is unreserved.
    /// Returns false if the swap failed.
    fn swap_reserved(&self, preferred: &Atom<LocalTree>, new: LocalTree, kind: Kind) {
        let old = preferred.swap(new);
        if old.present() {
            self.trees
                .unreserve(old.frame() / TREE_FRAMES, old.free(), kind);
        }
    }
}

impl fmt::Debug for LLFree<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct FmtFn<F>(F)
        where
            F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result;

        impl<F> fmt::Debug for FmtFn<F>
        where
            F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result,
        {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                (self.0)(f)
            }
        }

        let huge = self.frames() / (1 << HUGE_ORDER);
        let free = self.free_frames();
        let free_huge = self.free_huge();

        f.debug_struct("LLFree")
            .field(
                "managed",
                &FmtFn(|f| write!(f, "{} frames ({huge} huge)", self.frames())),
            )
            .field(
                "free",
                &FmtFn(|f| write!(f, "{free} frames ({free_huge} huge)")),
            )
            .field(
                "trees",
                &FmtFn(|f| write!(f, "{:?} (N={})", self.trees, TREE_FRAMES)),
            )
            .field("locals", &self.local)
            .finish()?;
        Ok(())
    }
}
