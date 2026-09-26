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

## Builder

The builder is not part of this repository, `tau-builder` crate is required to build the system and firmware.
Install the builder from separate repository.
This workspace provides `tau-firmware` in `0-M` with `tau-qemu` and `tau-visionfive2` binary targets, plus `tau-visionfive2-spl` in `0-M/vf2` for the board's first boot stage. The supervisor package lives in `2-S`, and the system package lives in `3-U/system`. The builder uses the binary target names when selecting and locating firmware images.

```
cargo install --path .
```

## Build for Vision Five 2

### Build Dependencies Ubuntu 24.04

```
apt install -y make clang llvm lld device-tree-compiler u-boot-tools
```

Build the Rust SPL, firmware and Tau with:

```
tau-builder build --vf2
```

Format the SD card. The command will ask root password. Double check device path,
the command will destroy the data contained on the first and last few megabytes of the device.
Then it will create GPT on the device and write the Rust SPL, rustsbi-based firmware
and tau on the corresponding partitions.

```
tau-builder format --path=/dev/sdX
```

In order to update only tau itself, keeping existing SPL and firmware, use this command:

```
tau-builder update --path=/dev/disk/by-partlabel/starfive_visionfive_2_tau
```

## Build for another computer

You need the device tree and firmware for the specific computer.

## Qemu

```
tau-builder build --qemu
qemu-system-riscv64 -M virt -smp 4 -m 4G -nographic -bios target/riscv64imac-unknown-none-elf/release/tau-qemu
```
