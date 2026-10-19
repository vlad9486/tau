// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later

//! Stable hart-local tags for retained roots. Successful contexts and their
//! ASIDs live until shutdown; migration and thread teardown are still TODO.

use core::{arch, fmt, hint, ptr};

use super::{
    asm,
    module::{ModuleContext, ModuleTables},
    vmem::{Root, Window, WindowAccess},
};

/// Exclusive references to the active root's context and module pages.
/// Moving this token into [`switch`] statically ends all borrows derived from
/// these root-dependent mappings before `satp` changes.
pub struct Local<'a> {
    pub module_context: &'a mut ModuleContext,
    pub module: &'a mut ModuleTables,
}

#[repr(C)]
pub struct Hart {
    mask: u16,
    flush: bool,
    next_asid: usize,
    pub hart_id: usize,
}

impl fmt::Display for Hart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Hart { mask, flush, .. } = self;
        writeln!(f, "ASID mask=0x{mask:04x} force_flush={flush}")
    }
}

pub fn init(hart_id: usize) -> Hart {
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
    Hart {
        mask,
        flush: mask == 0,
        next_asid: 1,
        hart_id,
    }
}

fn force_flush(window: &WindowAccess) -> bool {
    window.hart().flush
}

/// Two different roots sharing ASID zero always need a fence. Check the roots
/// first so an untagged MMU need not translate/load the separate hart page on
/// every switch. Keep the hart policy for tagged switches and forced flushing.
#[inline(always)]
pub fn switch_flush(window: &WindowAccess, source: Root, destination: Root) -> bool {
    if (source.asid() | destination.asid()) == 0 || force_flush(window) {
        hint::cold_path();
        true
    } else {
        false
    }
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
/// Both roots must share executing supervisor code, stack and hart mappings,
/// and `root` must map initialized module-context and module pages.
/// All global mappings must have identical backing and permissions on this hart.
/// Unique live ASIDs and ordered page-table publication are required without a fence.
#[inline(always)]
pub unsafe fn switch<'a>(local: Local<'a>, root: Root, flush: bool) -> Local<'a> {
    let module_context = ptr::from_mut(local.module_context);
    let module = ptr::from_mut(local.module);
    if flush {
        hint::cold_path();
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
    unsafe {
        Local {
            module_context: &mut *module_context,
            module: &mut *module,
        }
    }
}
