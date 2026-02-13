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

## Build Dependencies Ubuntu 24.04

```
apt install -y make clang llvm lld device-tree-compiler u-boot-tools
```

You may be missing some build time dependencies. It needs `git`, `make`, `clang` and `riscv64-gnu-toolchain-elf-bin` to build u-boot and OpenSBI.

## Build for Vision Five 2

To build the OS image, build the builder from corresponding repository:

```
cargo install --path tools --bin tau-builder
```

Run this command to clone u-boot and OpenSBI git repositories into `riscv/target` directory
and build them.

```
tau-builder build-firmware
```

Format the SD card. The command will ask root password. Double check device path,
the command will destroy the data contained on the first few megabytes of the disk.
Then it will create GPT on the device and write u-boot and OpenSBI on the corresponding partitions.

```
tau-builder format --path=/dev/sda
```

Build and write the tau image on the SD card by the following:

```
tau-builder build-tau
tau-builder update --path=/dev/disk/by-partlabel/starfive_visionfive_2_u-boot
```

## Build for another computer

You need the device tree and OpenSBI version for the specific computer.

## Qemu

```
tau-builder build-tau --qemu
qemu-system-riscv64 -M virt -smp 4 -m 4G -nographic -bios target/opensbi-qemu/build/platform/generic/firmware/fw_payload.elf
```
