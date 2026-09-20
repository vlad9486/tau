//! General utility functions

use core::{
    fmt,
    ops::{Deref, DerefMut},
};

/// Retries the condition n times and returns if it was successfull.
/// This pauses the CPU between retries if possible.
pub fn spin_wait(n: usize, mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..n {
        if cond() {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Calculate the size of a slice of T, respecting any alignment constraints
///
/// Note: This might not be correct for all types, but it is for the ones we use.
pub const fn size_of_slice<T>(len: usize) -> Option<usize> {
    len.checked_mul(size_of::<T>().next_multiple_of(align_of::<T>()))
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
