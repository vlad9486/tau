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

You may be missing some build time dependencies. It requires llvm binutils to produce debugging disassembly. It also needs `dtc` to compile the device tree and `clang` to build OpenSBI.

## Build

To build the OS image do:

```bash
make -C build target/tau
```

The file will be in `build/target/tau`, as you can see from the command. This file should be a payload for [OpenSBI](https://github.com/riscv-software-src/opensbi.git).

### Run in Qemu

```
make -C build run-qemu
```

## Build for Vision Five 2

Build the OS image:

```
make -C build target/vf2/tau.img
```

Copy to `/dev/sda2` and run `picocom` on `/dev/ttyUSB0`. Edit the makefile if you need to change these paths:

```
make -C build install-vf2
```

## Build for another computer

You need the device tree and OpenSBI version for the specific computer.
