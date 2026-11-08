// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{arch, hint, num::NonZero, ptr};

use super::{
    Call, Entry, Event, MemoryZone, ModuleError, ModuleHandle, PageFlags, SpawnError, WaitError,
    WaitSlot, AllocError, FreeError, ModuleCapabilities,
};

#[inline(always)]
fn ubi<const I: usize, const O: usize>(call: Call, arg: [usize; I]) -> (usize, [usize; O]) {
    const {
        assert!(I <= 5 && O <= 5);
    }

    let mut words = [0; 5];
    words[..I].copy_from_slice(&arg);
    let status;
    unsafe {
        arch::asm!(
            "ecall",
            inout("a0") call.encode() => status,
            inout("a1") words[0],
            inout("a2") words[1],
            inout("a3") words[2],
            inout("a4") words[3],
            inout("a5") words[4],
            clobber_abi("C"),
            options(nostack),
        );
    }
    let mut output = [0; O];
    output.copy_from_slice(&words[..O]);
    (status, output)
}

/// The system interface (User Binary Interface).
pub struct Ubi;

impl Ubi {
    /// Allocate `2^order` contiguous zeroed physical frames from `zone`.
    /// Requires `loader_authority`.
    pub fn alloc_physical(order: u8, zone: MemoryZone) -> Result<usize, AllocError> {
        let (r0, [phys]) = ubi(Call::AllocPhysical { order, zone }, []);
        if let Some(code) = NonZero::new(r0) {
            Err(unsafe { AllocError::decode(code) })
        } else {
            Ok(phys)
        }
    }

    /// Release `2^order` contiguous physical frames at `physical_addr` back to `zone`.
    /// Requires `loader_authority`.
    pub fn free_physical(
        physical_addr: usize,
        order: u8,
        zone: MemoryZone,
    ) -> Result<(), FreeError> {
        let (r0, []) = ubi(Call::FreePhysical { order, zone }, [physical_addr]);
        if let Some(code) = NonZero::new(r0) {
            Err(unsafe { FreeError::decode(code) })
        } else {
            Ok(())
        }
    }

    /// Create an empty module template and shared tables, returning its handle.
    /// Requires `loader_authority`.
    pub fn create_module(
        entry: usize,
        stack_pages: usize,
        wait_slots_addr: usize,
        wait_slots_count: usize,
        caps: ModuleCapabilities,
    ) -> Result<ModuleHandle, ModuleError> {
        let (r0, [handle]) = ubi(
            Call::CreateModule,
            [
                entry,
                stack_pages,
                wait_slots_addr,
                wait_slots_count,
                usize::from(caps.as_raw()),
            ],
        );
        if let Some(code) = NonZero::new(r0) {
            Err(unsafe { ModuleError::decode(code) })
        } else {
            Ok(ModuleHandle::from_raw(handle))
        }
    }

    /// Attach an existing module instance without copying its ELF or globals.
    /// Each call allocates a fresh parent-local slot. ROOT is valid only as a
    /// parent. The trusted loader must validate that this edge creates no cycle.
    pub fn bind_dependency(
        parent_module_id: ModuleHandle,
        dependency_module_id: ModuleHandle,
    ) -> Result<u16, ModuleError> {
        let (r0, [slot]) = ubi(
            Call::BindDependency,
            [parent_module_id.as_raw(), dependency_module_id.as_raw()],
        );
        if let Some(code) = NonZero::new(r0) {
            Err(unsafe { ModuleError::decode(code) })
        } else {
            Ok(slot as u16)
        }
    }

    /// Bind a software IRQ target `(hart_id, bit)` to `region_index` on `module`.
    /// `bit` must be in `1..64` (bit 0 is reserved for hardware interrupts).
    /// Requires `loader_authority`.
    pub fn bind_irq(
        module: ModuleHandle,
        region_index: usize,
        hart_id: usize,
        bit: u8,
    ) -> Result<(), ModuleError> {
        let (r0, []) = ubi(
            Call::BindIrq,
            [module.as_raw(), region_index, hart_id, usize::from(bit)],
        );
        if let Some(code) = NonZero::new(r0) {
            Err(unsafe { ModuleError::decode(code) })
        } else {
            Ok(())
        }
    }

    pub fn invoke<const I: usize, const O: usize>(
        slot: u16,
        arg: u16,
        msg: [usize; I],
    ) -> Result<[usize; O], NonZero<usize>> {
        let (r0, msg) = ubi(Call::Invoke { slot, arg }, msg);
        if let Some(error) = NonZero::new(r0) {
            Err(error)
        } else {
            Ok(msg)
        }
    }

    pub fn respond<const O: usize>(code: u16, msg: [usize; O]) -> ! {
        let (_, []) = ubi(Call::Respond { code, keep: false }, msg);
        unsafe { hint::unreachable_unchecked() }
    }

    pub fn respond_keep<const O: usize, const I: usize>(
        code: u16,
        msg: [usize; O],
    ) -> Result<[usize; I], NonZero<usize>> {
        let (r0, msg) = ubi(Call::Respond { code, keep: true }, msg);
        if let Some(error) = NonZero::new(r0) {
            Err(error)
        } else {
            Ok(msg)
        }
    }

    pub fn spawn<const I: usize>(entry: Entry, msg: [usize; I]) -> Result<(), SpawnError> {
        let entry = entry as usize;
        let (r0, []) = ubi(Call::Spawn { entry }, msg);
        if let Some(code) = NonZero::new(r0) {
            Err(unsafe { SpawnError::decode(code) })
        } else {
            Ok(())
        }
    }

    /// Start secondary hart `hart_id` in the bootstrap module.
    /// Requires `loader_authority`.
    pub fn start_hart(hart_id: usize) -> Result<(), SpawnError> {
        let (r0, []) = ubi(Call::StartHart, [hart_id]);
        if let Some(code) = NonZero::new(r0) {
            Err(unsafe { SpawnError::decode(code) })
        } else {
            Ok(())
        }
    }

    pub fn map(
        physical_addr: Option<NonZero<usize>>,
        virtual_addr: usize,
        number_of_pages: usize,
    ) -> Result<(), AllocError> {
        Self::map_module(
            ModuleHandle::ROOT,
            physical_addr,
            virtual_addr,
            number_of_pages,
            PageFlags::RW,
        )
    }

    pub fn map_module(
        target: ModuleHandle,
        physical_addr: Option<NonZero<usize>>,
        virtual_addr: usize,
        number_of_pages: usize,
        flags: PageFlags,
    ) -> Result<(), AllocError> {
        let physical_addr = physical_addr.map(NonZero::get).unwrap_or_default();
        let (r0, []) = ubi(
            Call::Map,
            [
                target.as_raw(),
                physical_addr,
                virtual_addr,
                number_of_pages,
                usize::from(flags.as_raw()),
            ],
        );
        if let Some(code) = NonZero::new(r0) {
            Err(unsafe { AllocError::decode(code) })
        } else {
            Ok(())
        }
    }

    pub fn unmap(virtual_addr: usize, number_of_pages: usize) -> Result<(), FreeError> {
        Self::unmap_module(ModuleHandle::ROOT, virtual_addr, number_of_pages)
    }

    pub fn unmap_module(
        target: ModuleHandle,
        virtual_addr: usize,
        number_of_pages: usize,
    ) -> Result<(), FreeError> {
        let (r0, []) = ubi(
            Call::Unmap,
            [target.as_raw(), virtual_addr, number_of_pages],
        );
        if let Some(code) = NonZero::new(r0) {
            Err(unsafe { FreeError::decode(code) })
        } else {
            Ok(())
        }
    }

    /// Consume readiness or suspend this thread, resuming here in this context.
    /// Shared-state publication/checking still needs module synchronization.
    pub fn wait(slot: &WaitSlot, deadline: Option<NonZero<usize>>) -> Result<Event, WaitError> {
        let deadline = deadline.map(NonZero::get).unwrap_or_default();
        let (r0, [event]) = ubi(Call::Wait, [ptr::from_ref(slot).addr(), deadline]);
        if let Some(code) = NonZero::new(r0) {
            Err(unsafe { WaitError::decode(code) })
        } else {
            Ok(Event(event as u64))
        }
    }

    /// Record coalesced readiness for a waiter in this module. Does not transfer
    /// this thread or execute code in the receiver's context.
    pub fn notify(slot: &WaitSlot) -> Result<(), WaitError> {
        let (r0, []) = ubi(Call::Notify, [ptr::from_ref(slot).addr()]);
        if let Some(code) = NonZero::new(r0) {
            Err(unsafe { WaitError::decode(code) })
        } else {
            Ok(())
        }
    }

    /// Record coalesced software irq for `region_index`.
    pub fn raise_irq(region_index: usize) -> Result<(), WaitError> {
        let (r0, []) = ubi(Call::RaiseIrq, [region_index]);
        if let Some(code) = NonZero::new(r0) {
            Err(unsafe { WaitError::decode(code) })
        } else {
            Ok(())
        }
    }

    /// Print a diagnostic checkpoint and one machine-word value through the
    /// supervisor's SBI debug console.
    pub fn debug(checkpoint: usize, value: usize) {
        let (_, []) = ubi(Call::Debug, [checkpoint, value]);
    }
}
