// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later

//! Stable hart-local tags for retained roots. Successful contexts and their
//! ASIDs live until shutdown; migration and thread teardown are still TODO.

use core::{arch, ptr};

use super::{
    asm,
    vmem::{Root, Window},
};

const HART: usize = 0xffff_ffc0_0000_6000;

#[repr(C)]
struct Hart {
    mask: u16,
    flush: bool,
    next_asid: usize,
}

fn hart() -> *mut Hart {
    ptr::without_provenance_mut(HART)
}

/// # Safety
/// Bootstrap must have mapped the private hart page; interrupts are disabled.
pub unsafe fn init() {
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
    unsafe {
        hart().write(Hart {
            mask,
            flush: false,
            next_asid: 1,
        });
    }
}

pub fn asid_mask() -> u16 {
    unsafe { (*hart()).mask }
}

pub fn force_flush() -> bool {
    (unsafe { (*hart()).flush }) || asid_mask() == 0
}

/// Allocate only after construction succeeds, so failed attempts consume no tag.
pub fn allocate_tag() -> Option<u16> {
    let hart = unsafe { &mut *hart() };
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
/// Unique live ASIDs and ordered page-table publication are required without a fence.
#[inline(always)]
pub unsafe fn switch(root: Root, flush: bool) {
    unsafe {
        arch::asm!("csrw satp, {0}", in(reg) root.0.get(), options(nostack));
    }
    if flush {
        asm::sfence_vma(None, None);
    }
}
