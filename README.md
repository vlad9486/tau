# Tau

Tau is a new operating system designed for RISC-V with a focus on high-performance, multi-core scalability, and security. Tau does not aim for POSIX compatibility but instead provides a clean-slate architecture optimized for modern software development.

## Key Features

### RISC-V Exclusive

Tau is built only for RISC-V and fully leverages its tagged TLBs for efficient address space switching.

### Designed for High-Core-Count Systems

Tau is optimized for 64+ core systems, ensuring superior scalability compared to Linux by minimizing contention points and leveraging lock-free data structures.

### Isolation by Default

* Each thread has its own address space, improving security and reducing the need for locks during memory allocation.
* Tagged MMU support enables lightweight and efficient context switching, improving performance for high-load applications.

### Lock-Free Kernel Components

* **Lock-free page frame allocation** for efficient memory management.
* **Lock-free scheduler** to maximize performance across many cores.

### Status

Tau is currently under development and not yet ready for general use.

## Tooling

The separate `tau-tool` repository provides the `tau` command for building firmware,
running OS tests, and preparing SD cards. Install it from the `tau-tool` checkout:

```
cargo install --path .
```

This workspace provides `tau-firmware` in `0-M` with `tau-qemu` and `tau-visionfive2` binary targets, plus `tau-visionfive2-spl` in `0-M/vf2` for the board's first boot stage. The supervisor package lives in `2-S`; U-mode modules live in `3-U/system`,
`3-U/test`, and `3-U/sdio`. Their static image layout is defined in `2-S/modules.toml`. The tool uses the binary target names when selecting and locating firmware images.

## Build for Vision Five 2

### Build Dependencies Ubuntu 24.04

```
apt install -y make clang llvm lld device-tree-compiler u-boot-tools
```

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

The image contains both `system` and `system-test`, plus a dummy `sdio` module.
`tau test` selects `system-test` through a loader feature. U-mode ELF copies are
stripped during composition; the original build artifacts retain debug info.
The loader copies the firmware DTB into a reserved 64 KiB buffer and adds
`/chosen/tau,modules` with U-mode module names, physical addresses, and reserved
byte lengths. Supervisor retains ELF and module initialization and forwards the
DTB address and page count using the original boot arguments. User programs find
modules with `Dtb::boot_module` and map them through `Area::sl`.
