CARGO_TARGET := riscv64gc-unknown-none-elf
RAW_BIN := ../target/$(CARGO_TARGET)/release
# must be in sync with linker scripts and `system/src/loader.rs`
SUPERVISOR_OFFSET := 5
SYSTEM_OFFSET := 16
LIMIT := 64

VF2_DT := jh7110-starfive-visionfive-2-v1.3b
VF2_ITS := visionfive2-uboot-fit-image.its

define list_rust_b
	$(1)/Cargo.toml $(1)/build.rs $(shell find $(1)/src -name '*.rs')
endef

define list_rust
	$(1)/Cargo.toml $(shell find $(1)/src -name '*.rs')
endef

clean: clean-vf2 clean-qemu
	rm target/{loader{,.dump},supervisor{,.dump},system{,.dump},tau} || true
	cargo clean

$(RAW_BIN)/loader: ../Cargo.lock $(call list_rust_b,../supervisor) $(call list_rust,../tau)
	RUSTFLAGS="-C relocation-model=pie" \
	cargo build --release --target=$(CARGO_TARGET) --features=panic-never \
		--package supervisor --bin loader

$(RAW_BIN)/supervisor: ../Cargo.lock $(call list_rust_b,../supervisor) $(call list_rust,../tau)
	RUSTFLAGS="" \
	cargo build --release --target=$(CARGO_TARGET) --features=panic-never \
		--package supervisor --bin supervisor

$(RAW_BIN)/system: ../Cargo.lock $(call list_rust_b,../system) $(call list_rust,../tau)
	RUSTFLAGS="" \
	cargo build --release --target=$(CARGO_TARGET) --package=system --bin=system

target/loader: $(RAW_BIN)/loader
	mkdir -p target
	llvm-objcopy --output-target=binary $(RAW_BIN)/loader target/loader
	llvm-objdump --print-imm-hex -d $(RAW_BIN)/loader > target/loader.dump

target/supervisor: $(RAW_BIN)/supervisor
	mkdir -p target
	llvm-objcopy --output-target=binary $(RAW_BIN)/supervisor target/supervisor
	llvm-objdump --print-imm-hex -d $(RAW_BIN)/supervisor > target/supervisor.dump

target/system: $(RAW_BIN)/system
	mkdir -p target
	cp $(RAW_BIN)/system target/system
	llvm-objdump --print-imm-hex -d $(RAW_BIN)/system > target/system.dump

target/tau: target/loader target/supervisor target/system
	dd if=/dev/zero of=target/tau bs=4k count=$(LIMIT)
	dd if=target/loader of=target/tau bs=4k conv=notrunc
	dd if=target/supervisor of=target/tau bs=4k conv=notrunc seek=$(SUPERVISOR_OFFSET)
	dd if=target/system of=target/tau bs=4k conv=notrunc seek=$(SYSTEM_OFFSET)

# QEMU

clean-qemu:
	rm -Rf target/qemu/*.elf || true
	cd target/qemu/opensbi && make distclean clean

target/qemu/opensbi:
	mkdir -p target/qemu && cd target/qemu && \
		git clone https://github.com/riscv-software-src/opensbi.git --depth 1

target/qemu/tau.elf: target/tau target/qemu/opensbi
	cd target/qemu/opensbi && \
		make CC=clang LLVM=1 LD=ld.lld PLATFORM=generic FW_PAYLOAD_PATH=../../tau \
		FW_TEXT_START=0x80000000 && \
		cp build/platform/generic/firmware/fw_payload.elf ../tau.elf

run-qemu: target/qemu/tau.elf board/qemu/qemu-virt.dtb
	qemu-system-riscv64 -M virt -smp 4 -m 8G -nographic \
		-bios target/qemu/tau.elf -dtb board/qemu/qemu-virt.dtb

# Vision Five 2

# StarFive recommends this OpenSBI revision in their main repo as submodule:
# https://github.com/starfive-tech/VisionFive2
# Do not use master — timer interrupts may not be delegated correctly!
VF2_OPENSBI_REVISION := 1725bd71080960290fdde4499a58c25c09d5c8ee

clean-vf2:
	rm -Rf target/vf2/*.img || true
	cd target/vf2/opensbi && make CC=clang LLVM=1 LD=ld.lld distclean clean

target/vf2/opensbi:
	mkdir -p target/vf2 && cd target/vf2 && \
		git clone https://github.com/starfive-tech/opensbi.git && \
		cd opensbi && git checkout $(VF2_OPENSBI_REVISION)

target/vf2/tau.img: target/tau target/vf2/opensbi board/vf2/$(VF2_DT).dtb board/vf2/$(VF2_ITS)
	cd target/vf2/opensbi && \
		make CC=clang LLVM=1 LD=ld.lld PLATFORM=generic \
		FW_FDT_PATH=../../../board/vf2/$(VF2_DT).dtb \
		FW_PAYLOAD_PATH=../../tau FW_TEXT_START=0x40000000
	cd target/vf2 && mkimage -f ../../board/vf2/$(VF2_ITS) tau.img

install-vf2: target/vf2/tau.img
	(dd if=target/vf2/tau.img of=/dev/disk/by-partlabel/starfive_visionfive_2_u-boot && sync) || \
		(sudo dd if=target/vf2/tau.img of=/dev/disk4s2 && diskutil unmountDisk /dev/disk4)
	# picocom /dev/ttyUSB0 -b 115200

# udev rule 
# KERNEL=="sd*[0-9]", ENV{ID_PART_ENTRY_NAME}=="tau", MODE="666"

# SRCTREE=...path/to/linux... \
	gcc -E -P -x assembler-with-cpp \
        -nostdinc -undef -D__DTS__ \
        -I"$SRCTREE/include" \
        -I"$SRCTREE/arch/riscv/boot/dts" \
        -I"$SRCTREE/arch/riscv/boot/dts/include" \
        -I"$SRCTREE/arch/riscv/boot/dts/starfive" \
        "$SRCTREE/arch/riscv/boot/dts/starfive/jh7110-starfive-visionfive-2-v1.3b.dts" | dtc -I dts -O dtb -o build/board/vf2/jh7110-starfive-visionfive-2-v1.3b.dtb
