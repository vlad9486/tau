// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Memory manager

use super::{
    llfree, csr,
    matter::{Module, ModuleDescriptor},
};

mod alloc;
pub use self::alloc::{map_pages, unmap_pages, validate_map, validate_unmap, validate_module_handle};

mod vmem;
pub use self::vmem::{
    Root, Mapping, MappingRange, Window, WindowAccess, PAGE, MODULE, MODULE_CONTEXT, ROOT_INDEX,
    BINDING_INDEX, user_range, addr_to_index, SCHEDULER, access,
};

// TODO: this is needed only for `loader`;
// encapsulate this in the `vmem` module, expose higher level functions.
pub use self::vmem::{
    Hart, flags_new, OWNED, HART_SLOT, STACK_SLOT, CONTEXT, SUPERVISOR_TEXT, ALLOCATOR,
    DMA_ALLOCATOR, MAX_RAM_FRAMES, MAX_DMA_FRAMES,
};
