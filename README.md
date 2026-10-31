# Tau

Tau is a new operating system designed for RISC-V with a focus on high-performance,
multi-core scalability, and security. Tau does not aim for POSIX compatibility but
instead provides a clean-slate architecture optimized for modern software development.

## Key Features

### RISC-V Exclusive

Tau is designed exclusively for RISC-V and takes full advantage of its tagged TLBs
(ASIDs) to enable efficient address-space switching.

### Unconventional Execution & IPC Model

* **Single-Thread Per Address Space**: Only one thread can execute in a given
  address space (`ModuleContext`) at a time, eliminating concurrent access within
  a context and reducing the need for locks during memory allocation.
* **Per-Thread Address-Space Trees**: Each thread has its own tree of address
  spaces (contexts). Because this requires more memory for page tables, userspace
  should prefer green threads over OS threads.
* **Two IPC Mechanisms**: Inter-process communication is performed exclusively
  via synchronous **thread migration** between address spaces (`Invoke`/`Respond`)
  and **shared memory**.
* **Static Dependency Classes & Runtime Installation**: Modules declare their
  dependency classes (defining possible IPC targets) statically *(currently not
  implemented properly)*. Modules can be installed and uninstalled at runtime,
  with dependencies verified at installation time rather than during IPC.

### Bounded Supervisor & $O(1)$ Design

The supervisor does not allocate privileged virtual memory dynamically and avoids
unbounded dynamic data structures. All supervisor structures live at fixed virtual
addresses (declared in the `extern "C"` block of `2-S/src/bin/supervisor.rs`)
whose scope is determined by the physical pages mapped into the active address space:

* `__WINDOW` (`space::Window`): Per-hart sparse virtual window for page-table
  walks, temporary physical page aliases, per-hart MMU state, and the supervisor stack.
* `__MODULE_CONTEXT` (`matter::ModuleContext`): Context-private execution state
  (saved registers, resume PC, caller `satp`, and cached callee dependency roots).
  The invocation chain is implicit via the caller's `satp`.
* `__MODULE` (`matter::Module`): Module-wide metadata shared across all contexts
  of a module, including prepared ELF stems, bound dependency slots, and wait-table
  records.
* `__SCHEDULER` (`scheduler::Scheduler`): System-wide scheduler state holding
  fixed-capacity thread tables and per-hart FIFO run queues.
* `__CONTEXT` (`state::Context`): System-wide lock-free physical frame allocators
  (`llfree`) and bootstrap metadata.

The supervisor is designed to perform all operations in $O(1)$ time, though a few
paths do not achieve this yet.

### Status

Tau is currently under development and not yet ready for general use.

## Tooling

The separate `tau-tool` repository provides the `tau` command for building firmware,
running OS tests, and preparing SD cards. Install it from the `tau-tool` checkout:

```
cargo install --path .
```

This workspace provides `tau-firmware` in `0-M` with `tau-qemu` and `tau-visionfive2`
binary targets, plus `tau-visionfive2-spl` in `0-M/vf2` for the board's first boot stage.
The supervisor package lives in `2-S`; U-mode modules live in `3-U/system`, `3-U/test`,
and `3-U/dummy-test`. Their static image layout is defined in `2-S/modules.toml`.
The tool uses the binary target names when selecting and locating firmware images.

## Build for Vision Five 2

Build the Rust SPL, firmware and Tau with:

```
tau build --vf2
```

Format the SD card. The command will ask root password. Double check device path,
the command will destroy the data contained on the first and last few megabytes of the device.
Then it will create GPT on the device and write the Rust SPL, rustsbi-based firmware
and tau on the corresponding partitions.

```
tau format --path=/dev/sdX
```

In order to update only tau itself, keeping existing SPL and firmware, use this command:

```
tau update --path=/dev/disk/by-partlabel/starfive_visionfive_2_tau
```

## Build for another computer

You need the device tree and firmware for the specific computer.

See `0-M/config.toml` as a starting point to port on the new computer.

## Qemu

```
tau build --qemu
qemu-system-riscv64 -M virt -smp 4 -m 4G -nographic -bios target/riscv64imac-unknown-none-elf/release/tau-qemu
```

## OS integration tests

Run `tau test` to build and boot the [system-test payload](3-U/test/README.md)
with the normal supervisor. Use `tau test --list` to list scenarios or
`tau test timer` to select one. Tests require QEMU and `gdb-multiarch`;
all host code lives in the separate `tau-tool` repository. Builds use the
default `target` directory; test logs live under `target/tau-test`. Run
`tau build --qemu` to switch the firmware back to the normal system payload.

The image contains both `system` and `system-test`, plus the `dummy-test` module.
`tau test` selects `system-test` through a loader feature. U-mode ELF copies are
stripped during composition; the original build artifacts retain debug info.
The loader copies the firmware DTB into a reserved 64 KiB buffer and adds
`/chosen/tau,modules` with U-mode module names, physical addresses, and reserved
byte lengths. Supervisor retains ELF and module initialization and forwards the
DTB address and page count using the original boot arguments. User programs find
modules with `Dtb::boot_module` and map them through `Area::sl`.
