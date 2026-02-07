//! Generic atomics

use core::fmt;
use core::sync::atomic::Ordering;
use core::sync::atomic::*;

/// Atomic value
///
/// See [core::sync::atomic::AtomicU64] for the documentation.
#[repr(transparent)]
pub struct Atom<T: Atomic>(pub T::I);

impl<T: Atomic> Atom<T> {
    pub fn new(v: T) -> Self {
        Self(T::I::new(v.into()))
    }

    #[track_caller]
    pub fn load(&self) -> T {
        self.0.load().into()
    }

    #[track_caller]
    pub fn store(&self, v: T) {
        self.0.store(v.into())
    }

    #[track_caller]
    pub fn swap(&self, v: T) -> T {
        self.0.swap(v.into()).into()
    }

    #[track_caller]
    pub fn compare_exchange(&self, current: T, new: T) -> Result<T, T> {
        match self.0.compare_exchange(current.into(), new.into()) {
            Ok(v) => Ok(v.into()),
            Err(v) => Err(v.into()),
        }
    }

    #[track_caller]
    pub fn fetch_update<F: FnMut(T) -> Option<T>>(&self, mut f: F) -> Result<T, T> {
        match self.0.fetch_update(|v| f(v.into()).map(Into::into)) {
            Ok(v) => Ok(v.into()),
            Err(v) => Err(v.into()),
        }
    }
}

impl<T: Atomic + Default> Default for Atom<T> {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl<T: Atomic + fmt::Debug> fmt::Debug for Atom<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.load().fmt(f)
    }
}

/// Types that can be converted from/into atomics
///
/// # Note
/// For compare_exchange and fetch_update, equality on this type has to be
/// the same as when they are converted into the underlying integers.
///
/// `a == b <-> a.into() == b.into()`
pub trait Atomic:
    Sized + Copy + Into<<Self::I as AtomicImpl>::V> + From<<Self::I as AtomicImpl>::V>
{
    type I: AtomicImpl;
}

/// Implementation of the atomic values
pub trait AtomicImpl: Sized {
    type V: Sized + Eq + Copy;

    fn new(v: Self::V) -> Self;

    fn load(&self) -> Self::V;

    fn store(&self, v: Self::V);

    fn swap(&self, v: Self::V) -> Self::V;

    fn compare_exchange(&self, current: Self::V, new: Self::V) -> Result<Self::V, Self::V>;

    fn fetch_update<F: FnMut(Self::V) -> Option<Self::V>>(&self, f: F) -> Result<Self::V, Self::V>;

    fn fetch_and(&self, v: Self::V) -> Self::V;

    fn fetch_or(&self, v: Self::V) -> Self::V;
}

macro_rules! atomic_trivial {
    ($($name:ident),+) => {
        $(
            fn $name(&self, v: Self::V) -> Self::V {
                self.$name(v.into(), Ordering::AcqRel).into()
            }
        )+
    };
}

macro_rules! fn_trivial {
    ($ty:ident ; $($name:ident),+) => {
        $(
            #[allow(dead_code)]
            pub fn $name(&self, v: $ty) -> $ty {
                AtomicImpl::$name(&self.0, v)
            }
        )+
    };
}

macro_rules! atomic_impl {
    ($ty:ident, $atomic:ident) => {
        impl Atomic for $ty {
            type I = $atomic;
        }

        impl AtomicImpl for $atomic {
            type V = $ty;

            fn new(v: Self::V) -> Self {
                Self::new(v)
            }

            fn load(&self) -> Self::V {
                self.load(Ordering::Acquire)
            }

            fn store(&self, v: Self::V) {
                self.store(v, Ordering::Release)
            }

            fn compare_exchange(&self, current: Self::V, new: Self::V) -> Result<Self::V, Self::V> {
                self.compare_exchange(current, new, Ordering::AcqRel, Ordering::Acquire)
            }

            fn fetch_update<F: FnMut(Self::V) -> Option<Self::V>>(
                &self,
                f: F,
            ) -> Result<Self::V, Self::V> {
                self.fetch_update(Ordering::AcqRel, Ordering::Acquire, f)
            }

            atomic_trivial![
                swap, fetch_and, fetch_or
            ];
        }

        impl Atom<$ty> {
            fn_trivial![
                $ty; fetch_and, fetch_or
            ];
        }
    };
}

atomic_impl!(u8, AtomicU8);
atomic_impl!(u16, AtomicU16);
atomic_impl!(u32, AtomicU32);
atomic_impl!(u64, AtomicU64);
atomic_impl!(usize, AtomicUsize);

pub trait AtomArray<T: Copy, const L: usize> {
    /// Overwrite the content of the whole array non-atomically.
    ///
    /// This is faster than atomics but does not handle race conditions.
    fn atomic_fill(&self, e: T);
}

impl<T: Atomic, const L: usize> AtomArray<T, L> for [Atom<T>; L] {
    fn atomic_fill(&self, e: T) {
        // cast to raw memory to let the compiler use vector instructions
        // WARNING: invalid cast, this is likely UB
        unsafe { &mut *(self.as_ptr() as *mut [T; L]) }.fill(e);
        // memory ordering has to be enforced with a memory barrier
        fence(Ordering::Release);
    }
}
