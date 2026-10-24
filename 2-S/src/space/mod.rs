// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Memory manager

use super::{
    llfree, csr,
    matter::{ModuleContext, Module, ModuleDescriptor},
    state::Context,
};

mod alloc;
pub use self::alloc::{
    map_pages, unmap_pages, validate_map, validate_map_unprivileged, validate_unmap,
    descriptor_page,
};

mod vmem;
pub use self::vmem::{
    Root, Mapping, Window, WindowAccess, PAGE, MODULE, MODULE_CONTEXT, user_range,
    SUPERVISOR_INDEX, SCHEDULER,
};

// TODO: this is needed only for `loader`;
// encapsulate this in the `vmem` module, expose higher level functions.
pub use self::vmem::{flags_new, OWNED, HART_SLOT, STACK_SLOT, ALLOCATOR};
