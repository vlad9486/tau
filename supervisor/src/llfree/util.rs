//! General utility functions

use core::{
    fmt,
    ops::{Deref, DerefMut},
};

/// Align v down to previous `align` (power of two!)
#[inline(always)]
pub const fn align_down(v: usize, align: usize) -> usize {
    (v / align) * align
}

/// Cache alignment for T
#[derive(Clone, Default, Hash, PartialEq, Eq)]
#[repr(align(64))]
pub struct Align<T = ()>(pub T);

const _: () = assert!(align_of::<Align>() == 64);
const _: () = assert!(align_of::<Align<usize>>() == 64);

impl<T> Deref for Align<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Align<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: fmt::Debug> fmt::Debug for Align<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}
