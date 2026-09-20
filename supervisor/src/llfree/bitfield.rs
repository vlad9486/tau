//! Atomic bitfield

use core::fmt;
use core::ops::Range;
use core::sync::atomic::AtomicU64;

use super::atomic::Atom;
use super::id::{FrameId, RowId};
use super::{Error, BITFIELD_ROW, HUGE_ORDER, ROWS};

/// Bitfield replacing the level one table.
pub struct Bitfield {
    data: [Atom<u64>; ROWS],
}

const _: () = assert!(size_of::<Bitfield>() >= 8);
const _: () = assert!(Bitfield::LEN.is_multiple_of(Bitfield::ROW_BITS));
const _: () = assert!(1 << Bitfield::ORDER == Bitfield::LEN);
const _: () = assert!(Bitfield::ORDER == HUGE_ORDER);

impl Default for Bitfield {
    fn default() -> Self {
        Self {
            data: [const { Atom(AtomicU64::new(0)) }; ROWS],
        }
    }
}

impl fmt::Debug for Bitfield {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bitfield( ")?;
        for d in &self.data {
            write!(f, "{:016x} ", d.load())?;
        }
        write!(f, ")")?;
        Ok(())
    }
}

impl Bitfield {
    const ROW_BITS: usize = BITFIELD_ROW;
    pub const LEN: usize = ROWS * Self::ROW_BITS;
    pub const ORDER: usize = Self::LEN.ilog2() as _;

    /// Overwrite the `range` of bits with `v`
    pub fn set(&self, range: Range<FrameId>, v: bool) {
        if range.start.0 >= range.end.0 || range.end.0 > Self::LEN {
            return;
        }
        let last = FrameId(range.end.0 - 1);
        if range.start.as_huge() != last.as_huge() {
            return;
        }

        if range.start != range.end {
            for ei in range.start.as_row().0..=last.as_row().0 {
                let ei = RowId(ei);
                let bit_off = ei.0 * Self::ROW_BITS;

                // Rows after the first one start at bit zero of that row.
                let bit_start = if ei == range.start.as_row() {
                    range.start.row_bit_idx()
                } else {
                    0
                };
                let bit_end = (range.end.0 - bit_off).min(Self::ROW_BITS);
                let bits = bit_end - bit_start;
                let row_mask = (u64::MAX >> (Self::ROW_BITS - bits)) << bit_start;
                if v {
                    self.row(ei).fetch_or(row_mask);
                } else {
                    self.row(ei).fetch_and(!row_mask);
                }
            }
        }
    }

    fn row(&self, i: RowId) -> &Atom<u64> {
        // huge_idx is modulo ROWS, so the index is always in bounds.
        &self.data[i.huge_idx()]
    }

    /// Return the  `i`-th row
    pub fn get_row(&self, i: RowId) -> u64 {
        self.row(i).load()
    }

    /// Toggle 2^`order` bits at the `i`-th place if they are all zero or one as expected
    ///
    /// Orders above 6 claim rows one at a time and roll back on contention.
    /// The caller must own every bit it frees; overlapping frees are invalid.
    pub fn toggle(&self, i: FrameId, order: usize, expected: bool) -> Result<(), Error> {
        if order > Self::ORDER || !i.is_aligned(order) {
            return Err(Error::InvalidArgument);
        }
        let i = FrameId(i.0 % Self::LEN);
        let num_bits = 1usize
            .checked_shl(order as u32)
            .ok_or(Error::OrderNotSuported)?;
        match order {
            0..=6 => {
                // Updates within a single row
                let mask = (u64::MAX >> (Self::ROW_BITS - num_bits)) << i.row_bit_idx();
                match self.row(i.as_row()).fetch_update(|e| {
                    if expected {
                        (e & mask == mask).then_some(e & !mask)
                    } else {
                        (e & mask == 0).then_some(e | mask)
                    }
                }) {
                    Ok(_) => Ok(()),
                    Err(_) => Err(Error::Address),
                }
            }
            _ => {
                // Update multiple rows
                let num_rows = num_bits / Self::ROW_BITS;
                let di = i.as_row().huge_idx();
                for i in di..di + num_rows {
                    let expected = if expected { !0 } else { 0 };
                    if let Err(_e) = self.row(RowId(i)).compare_exchange(expected, !expected) {
                        // log::warn!("Toggle failed {e:x} != {expected:x}");

                        // Undo changes
                        for j in (di..i).rev() {
                            // Allocation rollback owns these rows. Free rollback
                            // requires the caller to own the entire freed range.
                            if self
                                .row(RowId(j))
                                .compare_exchange(!expected, expected)
                                .is_err()
                            {
                                return Err(Error::UndoFailed);
                            }
                        }
                        return Err(Error::Address);
                    }
                }
                Ok(())
            }
        }
    }

    pub fn is_zero(&self, i: FrameId, order: usize) -> bool {
        if order > Self::ORDER || !i.is_aligned(order) {
            return false;
        }
        let i = FrameId(i.0 % Self::LEN);
        let Some(num_bits) = 1usize.checked_shl(order as u32) else {
            return false;
        };

        let row_i = i.as_row();
        if num_bits > Self::ROW_BITS {
            let end_i = FrameId(i.0 + num_bits).as_row();
            (row_i.0..end_i.0).all(|i| self.get_row(RowId(i)) == 0)
        } else {
            let row = self.get_row(row_i);
            let mask = (u64::MAX >> (u64::BITS as usize - num_bits)) << i.row_bit_idx();
            (row & mask) == 0
        }
    }

    /// Set the first aligned 2^`order` zero bits, returning the bit offset
    ///
    /// Orders above 6 claim rows one at a time and roll back on contention.
    /// The caller must own every bit it frees; overlapping frees are invalid.
    pub fn set_first_zeros(&self, start_row: RowId, order: usize) -> Result<FrameId, Error> {
        if order > Self::ORDER {
            return Err(Error::OrderNotSuported);
        }
        let small_order = match SmallOrder::new(order) {
            Ok(v) => v,
            Err(order) => {
                return self
                    .set_first_zero_rows(order)
                    .and_then(|row| row.as_frame().ok_or(Error::InvalidArgument));
            }
        };

        for i in 0..self.data.len() {
            let i = RowId(i + start_row.huge_idx()).huge_idx();

            let mut offset = FrameId(0);
            if self
                .row(RowId(i))
                .fetch_update(|e| {
                    let (val, o) = first_zeros_aligned(e, small_order)?;
                    offset = FrameId(o);
                    Some(val)
                })
                .is_ok()
            {
                return Ok(FrameId(i * Self::ROW_BITS + offset.0));
            }
        }
        Err(Error::Memory)
    }

    /// Allocate multiple rows with multiple CAS
    ///
    /// A candidate is returned only after every row has been claimed.
    fn set_first_zero_rows(&self, order: usize) -> Result<RowId, Error> {
        let num_rows = match order {
            7 => 2,
            8 => 4,
            9 => 8,
            _ => return Err(Error::OrderNotSuported),
        };

        for (i, rows) in self.data.chunks(num_rows).enumerate() {
            let row = RowId(i * num_rows);
            let frame = row.as_frame().ok_or(Error::InvalidArgument)?;
            if rows.iter().all(|row| row.load() == 0) && self.toggle(frame, order, false).is_ok() {
                return Ok(row);
            }
        }
        Err(Error::Memory)
    }

    /// Fill this bitset with `v` ignoring any previous data.
    pub fn fill(&self, v: bool) {
        let v = if v { u64::MAX } else { 0 };
        for row in &self.data {
            row.store(v);
        }
    }
}

#[derive(Clone, Copy)]
enum SmallOrder {
    O0,
    O1,
    O2,
    O3,
    O4,
    O5,
    O6,
}

impl SmallOrder {
    #[inline]
    fn new(v: usize) -> Result<Self, usize> {
        // Bitfield::ROW_BITS.ilog2() == 6
        match v {
            0 => Ok(Self::O0),
            1 => Ok(Self::O1),
            2 => Ok(Self::O2),
            3 => Ok(Self::O3),
            4 => Ok(Self::O4),
            5 => Ok(Self::O5),
            6 => Ok(Self::O6),
            v => Err(v),
        }
    }
}

/// Set the first aligned 2^`order` zero bits, returning the bit offset
///
/// - See <https://graphics.stanford.edu/~seander/bithacks.html#ZeroInWord>
fn first_zeros_aligned(v: u64, order: SmallOrder) -> Option<(u64, usize)> {
    match order {
        SmallOrder::O0 => {
            let off = v.trailing_ones();
            (off < u64::BITS).then(|| (v | (0b1 << off), off as _))
        }
        SmallOrder::O1 => {
            let mask = 0xaaaa_aaaa_aaaa_aaaa_u64;
            let off = ((v | (v >> 1)) | mask).trailing_ones();
            (off < u64::BITS).then(|| (v | (0b11 << off), off as _))
        }
        SmallOrder::O2 => {
            let mask = 0x1111_1111_1111_1111_u64;
            let off = (((v.wrapping_sub(mask) & !v) >> 3) & mask).trailing_zeros();
            (off < u64::BITS).then(|| (v | (0b1111 << off), off as _))
        }
        SmallOrder::O3 => {
            let mask = 0x0101_0101_0101_0101_u64;
            let off = (((v.wrapping_sub(mask) & !v) >> 7) & mask).trailing_zeros();
            (off < u64::BITS).then(|| (v | (0xff << off), off as _))
        }
        SmallOrder::O4 => {
            let mask = 0x0001_0001_0001_0001_u64;
            let off = (((v.wrapping_sub(mask) & !v) >> 15) & mask).trailing_zeros();
            (off < u64::BITS).then(|| (v | (0xffff << off), off as _))
        }
        SmallOrder::O5 => {
            let mask = 0xffff_ffff_u64;
            if v as u32 == 0 {
                Some((v | mask, 0))
            } else if v >> 32 == 0 {
                Some((v | (mask << 32), 32))
            } else {
                None
            }
        }
        SmallOrder::O6 => (v == 0).then_some((u64::MAX, 0)),
    }
}

#[cfg(test)]
mod test {
    use super::{FrameId, RowId};

    #[test]
    fn multirow_contention_rolls_back_before_trying_next_candidate() {
        let bitfield = super::Bitfield::default();
        bitfield.toggle(FrameId(64), 6, false).unwrap();
        assert!(bitfield.toggle(FrameId(0), 7, false).is_err());
        assert_eq!(bitfield.get_row(RowId(0)), 0);
        assert_eq!(bitfield.get_row(RowId(1)), u64::MAX);
        assert_eq!(bitfield.set_first_zeros(RowId(0), 7), Ok(FrameId(128)));
    }

    #[test]
    fn bit_set() {
        let bitfield = super::Bitfield::default();
        bitfield.set(FrameId(0)..FrameId(0), true);
        assert_eq!(bitfield.get_row(RowId(0)), 0);
        assert_eq!(bitfield.get_row(RowId(1)), 0);
        bitfield.set(FrameId(0)..FrameId(1), true);
        assert_eq!(bitfield.get_row(RowId(0)), 0b1);
        assert_eq!(bitfield.get_row(RowId(1)), 0b0);
        bitfield.set(FrameId(0)..FrameId(1), false);
        assert_eq!(bitfield.get_row(RowId(0)), 0b0);
        assert_eq!(bitfield.get_row(RowId(1)), 0b0);
        bitfield.set(FrameId(0)..FrameId(2), true);
        assert_eq!(bitfield.get_row(RowId(0)), 0b11);
        assert_eq!(bitfield.get_row(RowId(1)), 0b0);
        bitfield.set(FrameId(2)..FrameId(56), true);
        assert_eq!(bitfield.get_row(RowId(0)), 0x00ff_ffff_ffff_ffff);
        assert_eq!(bitfield.get_row(RowId(1)), 0b0);
        bitfield.set(FrameId(60)..FrameId(73), true);
        assert_eq!(bitfield.get_row(RowId(0)), 0xf0ff_ffff_ffff_ffff);
        assert_eq!(bitfield.get_row(RowId(1)), 0x01ff);
        bitfield.set(FrameId(96)..FrameId(128), true);
        assert_eq!(bitfield.get_row(RowId(0)), 0xf0ff_ffff_ffff_ffff);
        assert_eq!(bitfield.get_row(RowId(1)), 0xffff_ffff_0000_01ff);
        bitfield.set(FrameId(0)..FrameId(128), false);
        assert_eq!(bitfield.get_row(RowId(0)), 0);
        assert_eq!(bitfield.get_row(RowId(1)), 0);
    }

    #[test]
    fn bit_toggle() {
        let bitfield = super::Bitfield::default();

        assert!(bitfield.is_zero(FrameId(8), 3));
        bitfield.toggle(FrameId(8), 3, false).unwrap();
        assert_eq!(bitfield.get_row(RowId(0)), 0xff00);
        assert_eq!(bitfield.get_row(RowId(1)), 0);
        assert!(!bitfield.is_zero(FrameId(8), 3));

        assert!(bitfield.is_zero(FrameId(16), 2));
        bitfield.toggle(FrameId(16), 2, false).unwrap();
        assert_eq!(bitfield.get_row(RowId(0)), 0xf_ff00);
        assert_eq!(bitfield.get_row(RowId(1)), 0);
        assert!(!bitfield.is_zero(FrameId(16), 2));

        assert!(bitfield.is_zero(FrameId(20), 2));
        bitfield.toggle(FrameId(20), 2, false).unwrap();
        assert_eq!(bitfield.get_row(RowId(0)), 0xff_ff00);
        assert_eq!(bitfield.get_row(RowId(1)), 0);
        assert!(!bitfield.is_zero(FrameId(16), 2));

        assert!(!bitfield.is_zero(FrameId(8), 3));
        bitfield.toggle(FrameId(8), 3, false).expect_err("");
        bitfield.toggle(FrameId(8), 3, true).unwrap();
        bitfield.toggle(FrameId(16), 3, true).unwrap();
        assert_eq!(bitfield.get_row(RowId(0)), 0);
        assert_eq!(bitfield.get_row(RowId(1)), 0);
        assert!(bitfield.is_zero(FrameId(0), super::Bitfield::ORDER));

        bitfield.toggle(FrameId(0), 6, false).unwrap();
        assert_eq!(bitfield.get_row(RowId(0)), u64::MAX);
        assert_eq!(bitfield.get_row(RowId(1)), 0);
        bitfield.toggle(FrameId(64), 6, false).unwrap();
        assert_eq!(bitfield.get_row(RowId(0)), u64::MAX);
        assert_eq!(bitfield.get_row(RowId(1)), u64::MAX);
        bitfield.toggle(FrameId(0), 7, true).unwrap();
        assert_eq!(bitfield.get_row(RowId(0)), 0);
        assert_eq!(bitfield.get_row(RowId(1)), 0);
    }

    #[test]
    fn first_zeros_aligned() {
        use super::SmallOrder::*;
        use super::first_zeros_aligned as fza;

        assert_eq!(fza(0b0, O0), Some((0b1, 0)));
        assert_eq!(fza(0b0, O1), Some((0b11, 0)));
        assert_eq!(fza(0b0, O2), Some((0b1111, 0)));
        assert_eq!(fza(0b0, O3), Some((0xff, 0)));
        assert_eq!(fza(0b0, O4), Some((0xffff, 0)));
        assert_eq!(fza(0b0, O5), Some((0xffff_ffff, 0)));
        assert_eq!(fza(0b0, O6), Some((0xffff_ffff_ffff_ffff, 0)));

        assert_eq!(fza(0b1, O0), Some((0b11, 1)));
        assert_eq!(fza(0b1, O1), Some((0b1101, 2)));
        assert_eq!(fza(0b1, O2), Some((0xf1, 4)));
        assert_eq!(fza(0b1, O3), Some((0xff01, 8)));
        assert_eq!(fza(0b1, O4), Some((0xffff_0001, 16)));
        assert_eq!(fza(0b1, O5), Some((0xffff_ffff_0000_0001, 32)));
        assert_eq!(fza(0b1, O6), None);

        assert_eq!(fza(0b101, O0), Some((0b111, 1)));
        assert_eq!(fza(0b10011, O1), Some((0b1_1111, 2)));
        assert_eq!(fza(0x10f, O2), Some((0x1ff, 4)));
        assert_eq!(fza(0x100ff, O3), Some((0x1_ffff, 8)));
        assert_eq!(fza(0x0001_0000_ffff, O4), Some((0x0001_ffff_ffff, 16)));
        assert_eq!(
            fza(0x0000_0000_ff00_ff0f, O5),
            Some((0xffff_ffff_ff00_ff0f, 32))
        );
        assert_eq!(
            fza(0b1111_0000_1100_0011_1000_1111, O2),
            Some((0b1111_1111_1100_0011_1000_1111, 16))
        );

        // Upper bound
        assert_eq!(
            fza(0x7fff_ffff_ffff_ffff, O0),
            Some((0xffff_ffff_ffff_ffff, 63))
        );
        assert_eq!(fza(0xffff_ffff_ffff_ffff, O0), None);

        assert_eq!(
            fza(0x3fff_ffff_ffff_ffff, O1),
            Some((0xffff_ffff_ffff_ffff, 62))
        );
        assert_eq!(fza(0x7fff_ffff_ffff_ffff, O1), None);

        assert_eq!(
            fza(0x0fff_ffff_ffff_ffff, O2),
            Some((0xffff_ffff_ffff_ffff, 60))
        );
        assert_eq!(fza(0x1fff_ffff_ffff_ffff, O2), None);
        assert_eq!(fza(0x3fff_ffff_ffff_ffff, O2), None);

        assert_eq!(
            fza(0x00ff_ffff_ffff_ffff, O3),
            Some((0xffff_ffff_ffff_ffff, 56))
        );
        assert_eq!(fza(0x0fff_ffff_ffff_ffff, O3), None);
        assert_eq!(fza(0x1fff_ffff_ffff_ffff, O3), None);

        assert_eq!(
            fza(0x0000_ffff_ffff_ffff, O4),
            Some((0xffff_ffff_ffff_ffff, 48))
        );
        assert_eq!(fza(0x0001_ffff_ffff_ffff, O4), None);
        assert_eq!(fza(0x00ff_ffff_ffff_ffff, O4), None);

        assert_eq!(
            fza(0x0000_0000_ffff_ffff, O5),
            Some((0xffff_ffff_ffff_ffff, 32))
        );
        assert_eq!(fza(0x0000_0001_ffff_ffff, O5), None);
        assert_eq!(fza(0x0000_ffff_ffff_ffff, O5), None);

        assert_eq!(fza(0, O6), Some((0xffff_ffff_ffff_ffff, 0)));
        assert_eq!(fza(1, O6), None);
        assert_eq!(fza(0xa000_0000_0000_0000, O6), None);
    }

    #[test]
    fn first_zero_rows() {
        let bitfield = super::Bitfield::default();

        // 9
        assert!(bitfield.data.iter().all(|e| e.load() == 0));
        assert_eq!(FrameId(0), bitfield.set_first_zeros(RowId(0), 9).unwrap());
        assert!(bitfield.data.iter().all(|e| e.load() == u64::MAX));
        bitfield.toggle(FrameId(0), 9, true).unwrap();
        assert!(bitfield.data.iter().all(|e| e.load() == 0));

        assert_eq!(FrameId(0), bitfield.set_first_zeros(RowId(0), 7).unwrap());
        assert!(bitfield.data[0..2].iter().all(|e| e.load() == u64::MAX));

        assert_eq!(
            FrameId(4 * 64),
            bitfield.set_first_zeros(RowId(0), 8).unwrap()
        );
        assert!(bitfield.data[4..8].iter().all(|e| e.load() == u64::MAX));

        assert_eq!(
            FrameId(2 * 64),
            bitfield.set_first_zeros(RowId(0), 6).unwrap()
        );
        assert!(bitfield.get_row(RowId(2)) == u64::MAX);
        assert_eq!(
            FrameId(3 * 64),
            bitfield.set_first_zeros(RowId(0), 6).unwrap()
        );
        assert!(bitfield.get_row(RowId(3)) == u64::MAX);

        bitfield.set_first_zeros(RowId(0), 9).expect_err("no mem");
        bitfield.set_first_zeros(RowId(0), 8).expect_err("no mem");
        bitfield.set_first_zeros(RowId(0), 7).expect_err("no mem");
        bitfield.set_first_zeros(RowId(0), 6).expect_err("no mem");
    }
}
