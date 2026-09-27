use core::{
    arch::{asm, global_asm, naked_asm},
    hint, ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use rustsbi::{Console, Hsm, Physical, RustSBI as _, SbiRet, Timer};

const MAX_HARTS: usize = board::MAX_HARTS;

// The linker reserves one machine-mode stack per hart. Export the count from
// the board's Rust constant so it has a single source of truth.
global_asm!(
    ".globl __MAX_HARTS",
    ".equ __MAX_HARTS, {max_harts}",
    max_harts = const MAX_HARTS,
);
const STARTING: usize = usize::MAX;
const STOPPED: usize = 1;
const START_PENDING: usize = 2;
const STARTED: usize = 0;

const INTERRUPT_BIT: usize = 1 << (usize::BITS - 1);
const MACHINE_TIMER: usize = INTERRUPT_BIT | 7;
const ILLEGAL_INSTRUCTION: usize = 2;
const SUPERVISOR_ECALL: usize = 9;
const STIP: usize = 1 << 5;
const MTIE: usize = 1 << 7;

const CLINT_MTIMECMP: usize = 0x4000;

static HART_STATE: [AtomicUsize; MAX_HARTS] = [const { AtomicUsize::new(STOPPED) }; MAX_HARTS];
static HART_START: [AtomicUsize; MAX_HARTS] = [const { AtomicUsize::new(0) }; MAX_HARTS];
static HART_OPAQUE: [AtomicUsize; MAX_HARTS] = [const { AtomicUsize::new(0) }; MAX_HARTS];

// These words must be present in the raw image: all harts need them before one
// elected boot hart has cleared .bss. BOOT_READY is 0 before election, 1 while
// the boot hart initializes RAM, and 2 once secondaries may proceed.
#[unsafe(link_section = ".data.boot")]
static BOOT_READY: AtomicUsize = AtomicUsize::new(0);
#[unsafe(link_section = ".data.boot")]
static BOOT_HART: AtomicUsize = AtomicUsize::new(usize::MAX);

// The tau tool supplies this file through TAU_SBI_PAYLOAD. Keeping the bytes in
// their own section makes both the ELF and an objcopy-produced raw image carry
// the supervisor payload at __PAYLOAD.
#[used]
#[unsafe(link_section = ".payload")]
static PAYLOAD_IMAGE: [u8; include_bytes!(env!("TAU_SBI_PAYLOAD_PATH")).len()] =
    *include_bytes!(env!("TAU_SBI_PAYLOAD_PATH"));

unsafe extern "C" {
    static __BSS_START: u8;
    static __BSS_END: u8;
    static __STACK_TOP: u8;
    static __STACK_SIZE: u8;
    static __PAYLOAD: u8;
    static __CLINT: u8;
}

struct FirmwareConsole;

impl Console for FirmwareConsole {
    fn write(&self, bytes: Physical<&[u8]>) -> SbiRet {
        let Some(address) = physical_address(bytes.phys_addr_lo(), bytes.phys_addr_hi()) else {
            return SbiRet::invalid_param();
        };
        if address.checked_add(bytes.num_bytes()).is_none() {
            return SbiRet::invalid_param();
        }
        for offset in 0..bytes.num_bytes() {
            uart_write(unsafe { ptr::read_volatile((address + offset) as *const u8) });
        }
        SbiRet::success(bytes.num_bytes())
    }

    fn read(&self, _bytes: Physical<&mut [u8]>) -> SbiRet {
        // Input is deliberately left for the next iteration. DBCN reads are
        // non-blocking, so returning no bytes is a valid implementation.
        SbiRet::success(0)
    }

    fn write_byte(&self, byte: u8) -> SbiRet {
        uart_write(byte);
        SbiRet::success(0)
    }
}

struct FirmwareTimer;

impl Timer for FirmwareTimer {
    fn set_timer(&self, stime_value: u64) {
        clear_stip();
        unsafe { mtimecmp(current_hart()).write_volatile(stime_value) };
    }
}

struct FirmwareHsm;

impl Hsm for FirmwareHsm {
    fn hart_start(&self, hart_id: usize, start_addr: usize, opaque: usize) -> SbiRet {
        if !board::valid_hart(hart_id) {
            return SbiRet::invalid_param();
        }
        let Some(state) = HART_STATE.get(hart_id) else {
            return SbiRet::invalid_param();
        };
        if start_addr & 1 != 0 {
            return SbiRet::invalid_address();
        }

        match state.compare_exchange(STOPPED, STARTING, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => {
                HART_START[hart_id].store(start_addr, Ordering::Relaxed);
                HART_OPAQUE[hart_id].store(opaque, Ordering::Relaxed);
                state.store(START_PENDING, Ordering::Release);
                SbiRet::success(0)
            }
            Err(_) => SbiRet::already_available(),
        }
    }

    fn hart_stop(&self) -> SbiRet {
        HART_STATE[current_hart()].store(STOPPED, Ordering::Release);
        SbiRet::success(0)
    }

    fn hart_get_status(&self, hart_id: usize) -> SbiRet {
        if !board::valid_hart(hart_id) {
            return SbiRet::invalid_param();
        }
        HART_STATE
            .get(hart_id)
            .map(|state| {
                let state = state.load(Ordering::Acquire);
                SbiRet::success(if state == STARTING {
                    START_PENDING
                } else {
                    state
                })
            })
            .unwrap_or_else(SbiRet::invalid_param)
    }
}

#[repr(C)]
struct TrapFrame {
    registers: [usize; 32],
}

impl TrapFrame {
    const A0: usize = 10;
    const A1: usize = 11;
    const A6: usize = 16;
    const A7: usize = 17;
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.init")]
#[unsafe(naked)]
extern "C" fn _start() -> ! {
    naked_asm!(
        ".option push",
        ".option norelax",
        "la gp, __global_pointer$",
        ".option pop",
        // Check hart ID before assigning a stack or joining the boot election.
        "li t0, {first_hart}",
        "bltu a0, t0, 5f",
        "li t0, {max_harts}",
        "bgeu a0, t0, 5f",
        // Elect the first arriving hart before touching .bss. A platform is
        // allowed to release a nonzero hart first, unlike QEMU's current
        // behavior. BOOT_READY lives in the loaded raw image, not .bss.
        "la t0, {ready}",
        "li t1, 1",
        "amoswap.d.aq t2, t1, (t0)",
        "bnez t2, 3f",
        "la t0, {boot_hart}",
        "sd a0, 0(t0)",
        "la t0, __BSS_START",
        "la t1, __BSS_END",
        "1:",
        "bgeu t0, t1, 2f",
        "sd zero, 0(t0)",
        "addi t0, t0, 8",
        "j 1b",
        "2:",
        "fence rw, rw",
        "la t0, {ready}",
        "li t1, 2",
        "sd t1, 0(t0)",
        "j 4f",
        "3:",
        "la t0, {ready}",
        "ld t1, 0(t0)",
        "li t2, 2",
        "bne t1, t2, 3b",
        "fence r, rw",
        "4:",
        "la sp, __STACK_TOP",
        "la t0, __STACK_SIZE",
        "mul t0, t0, a0",
        "sub sp, sp, t0",
        "j {main}",
        "5:",
        "wfi",
        "j 5b",
        ready = sym BOOT_READY,
        boot_hart = sym BOOT_HART,
        main = sym firmware_main,
        first_hart = const board::FIRST_HART,
        max_harts = const MAX_HARTS,
    )
}

extern "C" fn firmware_main(hart_id: usize, _opaque: usize) -> ! {
    if !board::valid_hart(hart_id) {
        park_forever();
    }

    configure_hart();
    if hart_id == BOOT_HART.load(Ordering::Acquire) {
        HART_STATE[hart_id].store(STARTED, Ordering::Release);
        puts(board::BOOT_MESSAGE);
        put_hex(hart_id);
        puts(b"\r\n");
        // The raw image embeds the board DTB independently of the incoming a1.
        enter_supervisor(
            symbol_addr(&raw const __PAYLOAD),
            hart_id,
            board::DTB.as_ptr() as usize,
        )
    } else {
        park_hart(hart_id)
    }
}

fn configure_hart() {
    // CLINT compare registers may reset to zero; avoid an unsolicited timer
    // interrupt immediately after the first mret.
    unsafe { mtimecmp(current_hart()).write_volatile(u64::MAX) };
    unsafe {
        asm!("csrw mtvec, {0}", in(reg) trap_vector as *const () as usize);
        asm!("csrw medeleg, {0}", in(reg) 0xb109usize);
        asm!("csrw mideleg, {0}", in(reg) 0x222usize);
        asm!("csrw mie, {0}", in(reg) MTIE);
        // Tau reads time from both S-mode and U-mode. Both counter gates must
        // permit the TIME CSR; resetting satp does not initialize these CSRs.
        asm!("csrw mcounteren, {0}", in(reg) 2usize);
        asm!("csrw scounteren, {0}", in(reg) 2usize);

        // One TOR entry grants S-mode access to the complete physical address
        // space. Platform-specific PMP policy can replace this for real boards.
        asm!("csrw pmpaddr0, {0}", in(reg) usize::MAX >> 2);
        asm!("csrw pmpcfg0, {0}", in(reg) 0x0fusize);
    }
}

fn park_hart(hart_id: usize) -> ! {
    loop {
        if HART_STATE[hart_id].load(Ordering::Acquire) == START_PENDING {
            let start = HART_START[hart_id].load(Ordering::Relaxed);
            let opaque = HART_OPAQUE[hart_id].load(Ordering::Relaxed);
            HART_STATE[hart_id].store(STARTED, Ordering::Release);
            enter_supervisor(start, hart_id, opaque);
        }
        hint::spin_loop();
    }
}

fn enter_supervisor(start: usize, hart_id: usize, opaque: usize) -> ! {
    let machine_stack = stack_top(hart_id);
    unsafe {
        asm!(
            "csrw mscratch, t1",
            "csrw mepc, t0",
            "csrw satp, zero",
            "csrw sie, zero",
            "csrw sip, zero",
            "fence.i",
            "csrr t0, mstatus",
            "li t1, 0x1802",
            "not t1, t1",
            "and t0, t0, t1",
            "li t1, 0x800",
            "or t0, t0, t1",
            "csrw mstatus, t0",
            "mret",
            // Fixed inputs prevent the scratch registers and sequential moves
            // from overwriting the hart ID or HSM opaque argument.
            in("t1") machine_stack,
            in("t0") start,
            in("a0") hart_id,
            in("a1") opaque,
            options(noreturn),
        )
    }
}

#[unsafe(naked)]
extern "C" fn trap_vector() -> ! {
    naked_asm!(
        "csrrw sp, mscratch, sp",
        "addi sp, sp, -256",
        "sd ra,   8(sp)",
        "sd t0,  40(sp)",
        "csrr t0, mscratch",
        "sd t0,  16(sp)",
        "sd gp,  24(sp)",
        "sd tp,  32(sp)",
        "sd t1,  48(sp)",
        "sd t2,  56(sp)",
        "sd s0,  64(sp)",
        "sd s1,  72(sp)",
        "sd a0,  80(sp)",
        "sd a1,  88(sp)",
        "sd a2,  96(sp)",
        "sd a3, 104(sp)",
        "sd a4, 112(sp)",
        "sd a5, 120(sp)",
        "sd a6, 128(sp)",
        "sd a7, 136(sp)",
        "sd s2, 144(sp)",
        "sd s3, 152(sp)",
        "sd s4, 160(sp)",
        "sd s5, 168(sp)",
        "sd s6, 176(sp)",
        "sd s7, 184(sp)",
        "sd s8, 192(sp)",
        "sd s9, 200(sp)",
        "sd s10,208(sp)",
        "sd s11,216(sp)",
        "sd t3, 224(sp)",
        "sd t4, 232(sp)",
        "sd t5, 240(sp)",
        "sd t6, 248(sp)",
        "mv a0, sp",
        "call {handler}",
        "ld ra,   8(sp)",
        "ld gp,  24(sp)",
        "ld tp,  32(sp)",
        "ld t1,  48(sp)",
        "ld t2,  56(sp)",
        "ld s0,  64(sp)",
        "ld s1,  72(sp)",
        "ld a0,  80(sp)",
        "ld a1,  88(sp)",
        "ld a2,  96(sp)",
        "ld a3, 104(sp)",
        "ld a4, 112(sp)",
        "ld a5, 120(sp)",
        "ld a6, 128(sp)",
        "ld a7, 136(sp)",
        "ld s2, 144(sp)",
        "ld s3, 152(sp)",
        "ld s4, 160(sp)",
        "ld s5, 168(sp)",
        "ld s6, 176(sp)",
        "ld s7, 184(sp)",
        "ld s8, 192(sp)",
        "ld s9, 200(sp)",
        "ld s10,208(sp)",
        "ld s11,216(sp)",
        "ld t3, 224(sp)",
        "ld t4, 232(sp)",
        "ld t5, 240(sp)",
        "ld t6, 248(sp)",
        "ld t0,  40(sp)",
        "addi sp, sp, 256",
        "csrrw sp, mscratch, sp",
        "mret",
        handler = sym handle_trap,
    )
}

extern "C" fn handle_trap(frame: &mut TrapFrame) {
    let cause = read_csr::<0x342>();
    match cause {
        MACHINE_TIMER => {
            unsafe { mtimecmp(current_hart()).write_volatile(u64::MAX) };
            set_stip();
        }
        ILLEGAL_INSTRUCTION if board::handle_illegal_instruction(frame) => {}
        SUPERVISOR_ECALL => {
            let extension = frame.registers[TrapFrame::A7];
            let function = frame.registers[TrapFrame::A6];
            let params = [
                frame.registers[10],
                frame.registers[11],
                frame.registers[12],
                frame.registers[13],
                frame.registers[14],
                frame.registers[15],
            ];
            let ret = board::SBI.handle_ecall(extension, function, params);
            frame.registers[TrapFrame::A0] = ret.error;
            frame.registers[TrapFrame::A1] = ret.value;
            write_csr::<0x341>(read_csr::<0x341>() + 4);

            let hart_id = current_hart();
            if HART_STATE[hart_id].load(Ordering::Acquire) == STOPPED {
                park_hart(hart_id);
            }
        }
        _ => {
            puts(b"Tau RustSBI: unhandled trap hart=");
            put_hex(current_hart());
            puts(b" mcause=");
            put_hex(cause);
            puts(b" mepc=");
            put_hex(read_csr::<0x341>());
            puts(b" mtval=");
            put_hex(read_csr::<0x343>());
            puts(b"\r\n");
            park_forever();
        }
    }
}

fn physical_address(low: usize, high: usize) -> Option<usize> {
    if usize::BITS == 64 {
        (high == 0).then_some(low)
    } else {
        low.checked_add(high.checked_shl(32)?)
    }
}

fn uart_write(byte: u8) {
    board::uart_write(byte);
}

fn puts(bytes: &[u8]) {
    for &byte in bytes {
        uart_write(byte);
    }
}

fn put_hex(value: usize) {
    puts(b"0x");
    for shift in (0..16).rev() {
        uart_write(b"0123456789abcdef"[(value >> (shift * 4)) & 0xf]);
    }
}

fn mtimecmp(hart_id: usize) -> *mut u64 {
    (symbol_addr(&raw const __CLINT) + CLINT_MTIMECMP + hart_id * size_of::<u64>()) as *mut u64
}

fn stack_top(hart_id: usize) -> usize {
    symbol_addr(&raw const __STACK_TOP) - symbol_addr(&raw const __STACK_SIZE) * hart_id
}

fn symbol_addr(symbol: *const u8) -> usize {
    symbol.addr()
}

fn current_hart() -> usize {
    read_csr::<0xf14>()
}

fn clear_stip() {
    unsafe { asm!("csrc mip, {0}", in(reg) STIP) };
}

fn set_stip() {
    unsafe { asm!("csrs mip, {0}", in(reg) STIP) };
}

fn read_csr<const CSR: usize>() -> usize {
    let value;
    unsafe { asm!("csrr {0}, {csr}", out(reg) value, csr = const CSR) };
    value
}

fn write_csr<const CSR: usize>(value: usize) {
    unsafe { asm!("csrw {csr}, {0}", in(reg) value, csr = const CSR) };
}

fn park_forever() -> ! {
    loop {
        hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    puts(b"Tau RustSBI: panic\r\n");
    park_forever()
}
