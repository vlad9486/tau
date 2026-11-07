// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use super::{space, state::Context};

mod module;
pub use self::module::{
    Module, ModuleDescriptor, ModuleSpec, WaitTable, WaitRecord, Capabilities, IrqTarget,
    create_module, bind_dependency, bind_irq,
};

mod context;
pub use self::context::{ModuleContext, spawn, spawn_hart, invoke};
