# VisionFive 2 Rust SPL

This package builds the SPL ELF for a VisionFive 2 v1.3b with **8 GiB LPDDR4**, booting from **microSD**. From the workspace root, run `cargo build --release -p tau-visionfive2-spl`. The separate `tau-tool` repository provides `tau build --vf2` to build both SPL and firmware. `tau format` converts the ELFs into raw images in memory and adds the 0x400-byte StarFive header when writing the SD card.

The ROM loads the SPL at `0x08000000`. Hart 1 configures the PLLs, 8 GiB LPDDR4, UART0 and SDIO1. The SPL reads the first 4 MiB of GPT partition 2 into `0x40000000` using four-bit SD mode and 64-sector reads, then releases harts 1–4 into Tau's SBI firmware. UART0 reports the main stages at 115200 baud:

```text
Tau SPL: clocks
Tau SPL: DDR
Tau SPL: SD
Tau SPL: firmware
```

If SD loading fails, it reports `Tau SPL: SD failed` and halts. The firmware message is drained from UART before secondary harts enter RustSBI, so their output does not overlap it.

UART0 is initialized in assembly before any stack, shared SRAM data, or custom CSR access. This uses the board's GPIO5 TX mux, enables APB0 and UART0 clocks, only deasserts UART resets, and configures the 16550 registers for 115200 baud. Reset and initial UART status polls are bounded. Other U74 harts wait for the firmware-release flag and do not reconfigure UART0.

The code follows `starfive-tech/u-boot` revision `c4c67bb66ae6f41c98537d18cf5c3abc8b97b8e4` and the local raw-image patch. That tree identifies itself as U-Boot 2021.10. Compared with upstream `v2021.10`, the JH7110 CPU/PLL, VisionFive 2 board startup, DDR, clock and reset drivers are vendor additions; the vendor also modifies the DesignWare MMC driver to send the initial clock command. The Rust SPL carries the board startup, LPDDR4 8 GiB tables, SDIO1, GPT partition read and hart handoff. `tools/import_ddr.py` records how the LPDDR4 tables were imported; building does not require U-Boot source.

The SPL has booted Tau on the physical board. The image fits the ROM's 180048-byte limit. The embedded Tau DTB currently advertises 4 GiB of RAM, so Tau will use only 4 GiB until that DTB is updated.
