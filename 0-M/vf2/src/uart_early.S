/*
 * SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
 * SPDX-License-Identifier: GPL-3.0-or-later
 * Upstream license: GPL-2.0-or-later.
 * See LICENSE for the full license text.
 */

/* Initialize UART0 before Rust uses the SRAM stack or shared data.
 * The macro clobbers t0-t3 only. It needs no calls, stack, gp, or atomics.
 */
    .macro tau_uart_init
    /* GPIO5: DOEN=0, DOUT=20 (UART0 TX), same mux as board_init_f. */
    li t0, 0x13040000
    lw t1, 4(t0)
    li t2, -16129 /* ~0x3f00 */
    and t1, t1, t2
    sw t1, 4(t0)
    lw t1, 0x44(t0)
    li t2, -32513 /* ~0x7f00 */
    and t1, t1, t2
    li t2, 0x1400
    or t1, t1, t2
    sw t1, 0x44(t0)
    fence iorw, iorw

    /* APB0 gate (12); UART0 APB gate (145); UART0 oscillator gate (146). */
    li t0, 0x13020000
    li t2, 0x80000000
    lw t1, 0x30(t0)
    or t1, t1, t2
    sw t1, 0x30(t0)
    lw t1, 0x244(t0)
    or t1, t1, t2
    sw t1, 0x244(t0)
    lw t1, 0x248(t0)
    or t1, t1, t2
    sw t1, 0x248(t0)
    fence iorw, iorw

    /* Vendor ns16550_serial_probe only deasserts resets. Do not pulse them.
     * Reset IDs 83/84: assert2/status2 bits 19/20. Bounded status wait, as in
     * the vendor reset driver; continue even if acknowledgment never arrives.
     */
    li t2, 0x180000
    not t3, t2
    lw t1, 0x300(t0)
    and t1, t1, t3
    sw t1, 0x300(t0)
    fence iorw, iorw
    li t3, 10000
2:
    lw t1, 0x310(t0)
    and t1, t1, t2
    beq t1, t2, 3f
    addi t3, t3, -1
    bnez t3, 2b
3:
    /* ns16550_init + ns16550_setbrg, with a bounded initial TEMT wait.
     * DT: reg-shift=2, reg-io-width=4, baudclk=24 MHz. Divisor=13.
     */
    li t0, 0x10000000
    li t3, 100000
4:
    lw t1, 0x14(t0)
    andi t1, t1, 0x40
    bnez t1, 5f
    addi t3, t3, -1
    bnez t3, 4b
5:
    sw zero, 4(t0) /* IER=0; assumes ROM/reset leaves DLAB clear */
    li t1, 3
    sw t1, 0x10(t0) /* MCR: DTR | RTS */
    li t1, 7
    sw t1, 8(t0) /* FCR: enable and clear FIFOs */
    li t1, 3
    sw t1, 0x0c(t0) /* LCR: 8N1 */
    fence iorw, iorw
    li t1, 0x83
    sw t1, 0x0c(t0) /* DLAB on */
    fence iorw, iorw
    li t1, 13
    sw t1, 0(t0) /* DLL */
    sw zero, 4(t0) /* DLM */
    fence iorw, iorw
    li t1, 3
    sw t1, 0x0c(t0) /* DLAB off */
    fence iorw, iorw

    .endm
