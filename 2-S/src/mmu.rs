// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later

//! Stable hart-local tags for retained roots. Successful contexts and their
//! ASIDs live until shutdown; migration and thread teardown are still TODO.

use core::arch;

use super::{
    asm,
    vmem::{Root, Window, WindowAccess},
};

#[repr(C)]
pub(super) struct Hart {
    mask: u16,
    flush: bool,
    next_asid: usize,
}

/// # Safety
/// The window must belong to the executing hart; interrupts are disabled.
pub unsafe fn init(window: &mut WindowAccess) {
    let original = Window::current_root();
    let probe = Root::sv39(original.page(), u16::MAX);
    unsafe {
        arch::asm!("csrw satp, {0}", in(reg) probe.0.get(), options(nostack));
    }
    let mask = Window::current_root().asid();
    unsafe {
        arch::asm!("csrw satp, {0}", in(reg) original.0.get(), options(nostack));
    }
    asm::sfence_vma(None, None);
    *window.hart_mut() = Hart {
        mask,
        flush: false,
        next_asid: 1,
    };
}

pub fn asid_mask(window: &WindowAccess) -> u16 {
    window.hart().mask
}

pub fn force_flush(window: &WindowAccess) -> bool {
    let hart = window.hart();
    hart.flush || hart.mask == 0
}

/// Two different roots sharing ASID zero always need a fence. Check the roots
/// first so an untagged MMU need not translate/load the separate hart page on
/// every switch. Keep the hart policy for tagged switches and forced flushing.
#[inline(always)]
pub fn switch_flush(window: &WindowAccess, source: Root, destination: Root) -> bool {
    (source.asid() | destination.asid()) == 0 || force_flush(window)
}

/// Allocate only after construction succeeds, so failed attempts consume no tag.
pub fn allocate_tag(window: &mut WindowAccess) -> Option<u16> {
    let hart = window.hart_mut();
    if hart.mask == 0 {
        Some(0)
    } else if hart.next_asid <= hart.mask as usize {
        let tag = hart.next_asid as u16;
        hart.next_asid += 1;
        Some(tag)
    } else {
        None
    }
}

/// # Safety
/// Both roots must share executing supervisor code, stack and hart mappings.
/// All global mappings must have identical backing and permissions on this hart.
/// Unique live ASIDs and ordered page-table publication are required without a fence.
#[inline(always)]
pub unsafe fn switch(root: Root, flush: bool) {
    if flush {
        // Keep the write and fence together: no compiler-generated access to
        // context storage may slip between changing roots and invalidation.
        // A non-x0 ASID operand preserves global translations, even for ASID 0.
        // Flush the destination tag when policy also requests a tagged switch.
        unsafe {
            arch::asm!(
                "csrw satp, {root}",
                "sfence.vma zero, t0",
                root = in(reg) root.0.get(),
                in("t0") usize::from(root.asid()),
                options(nostack),
            );
        }
    } else {
        unsafe {
            arch::asm!("csrw satp, {0}", in(reg) root.0.get(), options(nostack));
        }
    }
}
