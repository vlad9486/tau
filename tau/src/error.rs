// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{hint, num::NonZero};

pub const ERR_UNSUPPORTED: usize = 1;
pub const ERR_PERMISSION: usize = 2;
pub const ERR_OUTOFMEMORY: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum WaitError {
    Unsupported = ERR_UNSUPPORTED,
    AccessDenied = ERR_PERMISSION,
    OutOfMemory = ERR_OUTOFMEMORY,
    InvalidWaiter = 0x100,
    Busy = 0x101,
}

impl WaitError {
    /// # Safety
    /// The code must be in range of `Self`
    #[inline]
    pub const unsafe fn decode(code: NonZero<usize>) -> Self {
        match code.get() {
            ERR_UNSUPPORTED => Self::Unsupported,
            ERR_PERMISSION => Self::AccessDenied,
            ERR_OUTOFMEMORY => Self::OutOfMemory,
            0x100 => Self::InvalidWaiter,
            0x101 => Self::Busy,
            _ => unsafe { hint::unreachable_unchecked() },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum SpawnError {
    OutOfMemory = ERR_OUTOFMEMORY,
    InvalidEntry = 0x100,
    NoSlots = 0x101,
}

impl SpawnError {
    /// # Safety
    /// The code must be in range of `Self`
    #[inline]
    pub const unsafe fn decode(code: NonZero<usize>) -> Self {
        match code.get() {
            ERR_OUTOFMEMORY => Self::OutOfMemory,
            0x100 => Self::InvalidEntry,
            0x101 => Self::NoSlots,
            _ => unsafe { hint::unreachable_unchecked() },
        }
    }
}
/// Construction/invocation status. Module authority is granted by bootstrap,
/// never inferred from an ELF's name or inherited from the invoking thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleError {
    AccessDenied,
    OutOfMemory,
    InvalidImage,
    NoSlots,
    InvalidDependency,
    Poisoned,
    Excpetion(u16),
}

impl ModuleError {
    #[inline]
    pub const fn encode(self) -> usize {
        match self {
            Self::AccessDenied => ERR_PERMISSION,
            Self::OutOfMemory => ERR_OUTOFMEMORY,
            Self::InvalidImage => 0x100,
            Self::NoSlots => 0x101,
            Self::InvalidDependency => 0x102,
            Self::Poisoned => 0x8040,
            Self::Excpetion(e) => 0x8000 | (e as usize),
        }
    }

    /// # Safety
    /// The code must be in range of `Self`
    #[inline]
    pub const unsafe fn decode(code: NonZero<usize>) -> Self {
        if code.get() == 0x8040 {
            return Self::Poisoned;
        }
        if code.get() & 0x8000 == 0x8000 {
            return Self::Excpetion((code.get() & 0x7fff) as u16);
        }
        match code.get() {
            ERR_PERMISSION => Self::AccessDenied,
            ERR_OUTOFMEMORY => Self::OutOfMemory,
            0x100 => Self::InvalidImage,
            0x101 => Self::NoSlots,
            0x102 => Self::InvalidDependency,
            _ => unsafe { hint::unreachable_unchecked() },
        }
    }
}

#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocError {
    AlreadyAllocated = 1,
    OutOfMemory = 2,
    AccessDenied = 3,
    InvalidAddress = 4,
}

impl AllocError {
    /// # Safety
    /// The code must be in range of `Self`
    #[inline]
    pub const unsafe fn decode(code: NonZero<usize>) -> Self {
        match code.get() {
            1 => Self::AlreadyAllocated,
            2 => Self::OutOfMemory,
            3 => Self::AccessDenied,
            4 => Self::InvalidAddress,
            _ => unsafe { hint::unreachable_unchecked() },
        }
    }
}

#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeError {
    AlreadyFree = 1,
    AccessDenied = 2,
    InvalidAddress = 3,
}

impl FreeError {
    /// # Safety
    /// The code must be in range of `Self`
    #[inline]
    pub const unsafe fn decode(code: NonZero<usize>) -> Self {
        match code.get() {
            1 => Self::AlreadyFree,
            2 => Self::AccessDenied,
            3 => Self::InvalidAddress,
            _ => unsafe { hint::unreachable_unchecked() },
        }
    }
}
