/* SPDX-License-Identifier: GPL-2.0+ */
/* Hart 1 initializes the board; the other U74 harts wait for firmware. */
    csrw mie, zero
    csrci mstatus, 8
    csrr a0, mhartid
    li t0, 1
    bne a0, t0, .Lsecondary

    tau_uart_init
    li a1, 1
    j .Lstack

.Lsecondary:
    /* Other U74 harts wait in Rust for BOOT_STATE=3 and never access UART0. */
    li t0, 2
    bltu a0, t0, .Lpark
    li t0, 5
    bgeu a0, t0, .Lpark
    li a1, 0

.Lstack:
    .option push
    .option norelax
    la gp, __global_pointer$
    .option pop
    li sp, 0x08180000
    slli t0, a0, 14
    sub sp, sp, t0
    j {main}
.Lpark:
    wfi
    j .Lpark
