// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Validated, immutable module wait-table backing. Record access requires the
//! scheduler lock; temporary window pointers never leave these operations.
use core::sync::atomic::{AtomicU8, Ordering};
use tau::{ModuleError, WaitError};
use super::{
    state::Context,
    vmem::{self, Root, WindowAccess, PAGE},
};

#[derive(Clone, Copy)]
#[repr(C)]
pub struct WaitTable {
    pub base: usize,
    pub count: usize,
    records: usize,
    order: usize,
}

#[derive(Clone, Copy, Default)]
#[repr(C, align(32))]
pub struct WaitRecord {
    pub pending: bool,
    pub thread: usize,
    pub sequence: usize,
}

impl WaitTable {
    pub const EMPTY: Self = Self {
        base: 0,
        count: 0,
        records: 0,
        order: 0,
    };

    pub fn prepare(
        window: &mut WindowAccess,
        context: &Context,
        hart: usize,
        root: Root,
        stem: usize,
        base: usize,
        count: usize,
    ) -> Result<Self, ModuleError> {
        if count == 0 {
            return Ok(Self::EMPTY);
        }
        let bytes = count.checked_mul(64).ok_or(ModuleError::InvalidImage)?;
        if base == 0 || count == 0 || base & 63 != 0 || !vmem::user_range(base, bytes) {
            return Err(ModuleError::InvalidImage);
        }
        let end = base.checked_add(bytes).ok_or(ModuleError::InvalidImage)?;
        if (base >> 30) & 511 != stem || ((end - 1) >> 30) & 511 != stem {
            return Err(ModuleError::InvalidImage);
        }
        for address in (base & !(PAGE - 1)..end).step_by(PAGE) {
            let (_, flags) = window
                .resolve(root, address)
                .ok_or(ModuleError::InvalidImage)?;
            if flags & 30 != 22 {
                return Err(ModuleError::InvalidImage);
            }
        }
        let storage = count
            .checked_mul(size_of::<WaitRecord>())
            .ok_or(ModuleError::InvalidImage)?;
        let pages = storage
            .div_ceil(PAGE)
            .checked_next_power_of_two()
            .ok_or(ModuleError::OutOfMemory)?;
        let order = pages.ilog2() as usize;
        let records = context
            .block(hart, order)
            .map_err(|_| ModuleError::OutOfMemory)?;
        for page in 0..pages {
            unsafe {
                window
                    .page_pointer(records + page * PAGE)
                    .write_bytes(0, PAGE)
            };
        }
        Ok(Self {
            base,
            count,
            records,
            order,
        })
    }

    /// Used only when unpublished module construction rolls back.
    pub fn release(self, context: &Context, hart: usize) {
        if self.records != 0 {
            let _ = context.free_block(hart, self.records, self.order);
        }
    }

    pub fn index(self, pointer: usize) -> Result<usize, WaitError> {
        let offset = pointer
            .checked_sub(self.base)
            .ok_or(WaitError::InvalidWaiter)?;
        if self.count == 0 || offset & 63 != 0 || offset / 64 >= self.count {
            return Err(WaitError::InvalidWaiter);
        }
        Ok(offset / 64)
    }

    pub fn occupied(
        self,
        window: &mut WindowAccess,
        root: Root,
        index: usize,
    ) -> Result<bool, WaitError> {
        let (physical, flags) = window
            .resolve(root, self.base + index * 64 + 63)
            .ok_or(WaitError::InvalidWaiter)?;
        if flags & 30 != 22 {
            return Err(WaitError::InvalidWaiter);
        }
        let byte = unsafe {
            &*window
                .wait_pointer(physical & !(PAGE - 1))
                .add(physical & (PAGE - 1))
                .cast::<AtomicU8>()
        }
        .load(Ordering::SeqCst);
        match byte {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WaitError::InvalidWaiter),
        }
    }

    /// Read one record through a temporary physical alias.
    ///
    /// # Safety
    /// The table must be retained and `index < count`. The scheduler lock must
    /// protect record access. Records fit wholly within individual backing pages.
    pub unsafe fn read(self, window: &mut WindowAccess, index: usize) -> WaitRecord {
        debug_assert!(index < self.count);
        let address = self.records + index * size_of::<WaitRecord>();
        unsafe {
            window
                .wait_pointer(address & !(PAGE - 1))
                .add(address & (PAGE - 1))
                .cast::<WaitRecord>()
                .read()
        }
    }

    /// Write one record through a temporary physical alias.
    ///
    /// # Safety
    /// The table must be retained and `index < count`. The scheduler lock must
    /// protect record access; no alias reference may survive this operation.
    pub unsafe fn write(self, window: &mut WindowAccess, index: usize, record: WaitRecord) {
        debug_assert!(index < self.count);
        let address = self.records + index * size_of::<WaitRecord>();
        unsafe {
            window
                .wait_pointer(address & !(PAGE - 1))
                .add(address & (PAGE - 1))
                .cast::<WaitRecord>()
                .write(record)
        };
    }
}
