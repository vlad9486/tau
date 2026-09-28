// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// Upstream license: GPL-2.0-or-later.
// See LICENSE for the full license text.

// Minimal DW MMC PIO boot path for the VisionFive 2 microSD slot.

use super::{
    delay_us,
    mmio::{read, write},
};

const SD: usize = 0x1602_0000;
const CTRL: usize = 0x00;
const PWREN: usize = 0x04;
const CLKDIV: usize = 0x08;
const CLKSRC: usize = 0x0c;
const CLKENA: usize = 0x10;
const TMOUT: usize = 0x14;
const CTYPE: usize = 0x18;
const BLKSIZ: usize = 0x1c;
const BYTCNT: usize = 0x20;
const INTMASK: usize = 0x24;
const CMDARG: usize = 0x28;
const CMD: usize = 0x2c;
const RESP0: usize = 0x30;
const RINTSTS: usize = 0x44;
const STATUS: usize = 0x48;
const FIFOTH: usize = 0x4c;
const DATA: usize = 0x200;
const CMD_DONE: u32 = 1 << 2;
const DATA_DONE: u32 = 1 << 3;
const ERRORS: u32 = 0xbfc2;
const RESPONSE: u32 = 1 << 6;
const CRC: u32 = 1 << 8;
const DATA_EXPECTED: u32 = 1 << 9;
const ABORT_STOP: u32 = 1 << 14;
const FIRMWARE_SECTORS: u32 = 8192;
const READ_CHUNK_SECTORS: u32 = 64;

#[derive(Debug)]
pub enum Error {
    Timeout,
    Controller,
    Card,
    Gpt,
    Partition,
}

fn wait_until(mut predicate: impl FnMut() -> bool, timeout_us: u64) -> Result<(), Error> {
    let start = unsafe { (0x0200_bff8 as *const u64).read_volatile() };
    while !predicate() {
        let now = unsafe { (0x0200_bff8 as *const u64).read_volatile() };
        if now.wrapping_sub(start) > timeout_us * 4 {
            return Err(Error::Timeout);
        }
        core::hint::spin_loop();
    }
    Ok(())
}

fn clock(khz: u32) -> Result<(), Error> {
    wait_until(|| read(SD + STATUS) & (1 << 9) == 0, 100_000)?;
    write(SD + CLKENA, 0);
    write(SD + CLKSRC, 0);
    write(SD + CLKDIV, 50_000u32.div_ceil(2 * khz));
    update_clock()?;
    write(SD + CLKENA, 1 | (1 << 16));
    update_clock()
}

fn update_clock() -> Result<(), Error> {
    write(SD + CMD, (1 << 31) | (1 << 21) | (1 << 13));
    wait_until(|| read(SD + CMD) & (1 << 31) == 0, 100_000)
}

fn command(code: u32, arg: u32, flags: u32) -> Result<u32, Error> {
    write(SD + RINTSTS, u32::MAX);
    write(SD + CMDARG, arg);
    unsafe { core::arch::asm!("fence ow, ow") };
    let wait_for_data = if code == 12 { 0 } else { 1 << 13 };
    write(
        SD + CMD,
        (1 << 31) | (1 << 29) | wait_for_data | code | flags,
    );
    wait_until(|| read(SD + RINTSTS) & (CMD_DONE | ERRORS) != 0, 1_000_000)?;
    let status = read(SD + RINTSTS);
    if status & ERRORS != 0 {
        return Err(Error::Controller);
    }
    Ok(read(SD + RESP0))
}

fn init() -> Result<bool, Error> {
    write(SD + PWREN, 0);
    delay_us(200_000);
    write(SD + PWREN, 1);
    delay_us(200_000);
    write(SD + CTRL, 7);
    wait_until(|| read(SD + CTRL) & 7 == 0, 100_000)?;
    write(SD + RINTSTS, u32::MAX);
    write(SD + INTMASK, 0);
    write(SD + CTRL, 0);
    clock(400)?;
    write(SD + BLKSIZ, 512);
    write(SD + CTYPE, 0);
    write(SD + TMOUT, 0xffff_ff40);
    write(SD + FIFOTH, (2 << 28) | (15 << 16) | 16);
    command(0, 0, 1 << 15)?;
    let response = command(8, 0x1aa, RESPONSE | CRC)?;
    if response & 0xfff != 0x1aa {
        return Err(Error::Card);
    }
    let mut ocr = 0;
    for _ in 0..200 {
        command(55, 0, RESPONSE | CRC)?;
        ocr = command(41, 0xc0ff_8000, RESPONSE)?;
        if ocr & (1 << 31) != 0 {
            break;
        }
        delay_us(5_000);
    }
    if ocr & (1 << 31) == 0 {
        return Err(Error::Card);
    }
    command(2, 0, RESPONSE | CRC | (1 << 7))?;
    let rca = command(3, 0, RESPONSE | CRC)? & 0xffff_0000;
    clock(25_000)?;
    command(7, rca, RESPONSE | CRC)?;
    wait_until(
        || command(13, rca, RESPONSE | CRC).is_ok_and(|status| (status >> 9) & 0xf == 4),
        1_000_000,
    )?;
    // Match U-Boot's SD bus selection: ACMD6 switches the card, then CTYPE
    // switches the controller. The VisionFive 2 SD slot wires all four bits.
    command(55, rca, RESPONSE | CRC)?;
    command(6, 2, RESPONSE | CRC)?;
    write(SD + CTYPE, 1);
    if ocr & (1 << 30) == 0 {
        command(16, 512, RESPONSE | CRC)?;
    }
    Ok(ocr & (1 << 30) != 0)
}

fn read_blocks(lba: u32, blocks: u32, target: *mut u32, high_capacity: bool) -> Result<(), Error> {
    debug_assert!(blocks > 0);
    let arg = if high_capacity {
        lba
    } else {
        lba.checked_mul(512).ok_or(Error::Card)?
    };
    write(SD + CTRL, 1 << 1);
    wait_until(|| read(SD + CTRL) & (1 << 1) == 0, 100_000)?;
    write(SD + BYTCNT, blocks * 512);
    command(
        if blocks == 1 { 17 } else { 18 },
        arg,
        RESPONSE | CRC | DATA_EXPECTED,
    )?;
    let mut words = 0;
    let total_words = blocks * 128;
    while words < total_words {
        wait_until(
            || {
                let bits = read(SD + RINTSTS);
                bits & ERRORS != 0 || (read(SD + STATUS) >> 17) & 0x1fff != 0
            },
            1_000_000,
        )?;
        if read(SD + RINTSTS) & ERRORS != 0 {
            return Err(Error::Controller);
        }
        let count = ((read(SD + STATUS) >> 17) & 0x1fff).min(total_words - words);
        for _ in 0..count {
            unsafe {
                target.add(words as usize).write_volatile(read(SD + DATA));
            }
            words += 1;
        }
    }
    wait_until(|| read(SD + RINTSTS) & (DATA_DONE | ERRORS) != 0, 1_000_000)?;
    if read(SD + RINTSTS) & ERRORS != 0 {
        return Err(Error::Controller);
    }
    if blocks > 1 {
        // U-Boot ends CMD18 with CMD12 after the controller reports DTO.
        command(12, 0, RESPONSE | CRC | ABORT_STOP)?;
    }
    write(SD + RINTSTS, u32::MAX);
    Ok(())
}

fn read_sector(lba: u32, target: *mut u32, high_capacity: bool) -> Result<(), Error> {
    read_blocks(lba, 1, target, high_capacity)
}

fn le_u32(words: &[u32; 128], byte: usize) -> u32 {
    words[byte / 4].to_le()
}

pub fn load_firmware() -> Result<usize, Error> {
    let high_capacity = init()?;
    let mut sector = [0u32; 128];
    read_sector(1, sector.as_mut_ptr(), high_capacity)?;
    if sector[0] != 0x2049_4645 || sector[1] != 0x5452_4150 {
        return Err(Error::Gpt);
    }
    if le_u32(&sector, 0x4c) != 0 {
        return Err(Error::Gpt);
    }
    let entries_lba = le_u32(&sector, 0x48);
    let entry_size = le_u32(&sector, 0x54);
    if entry_size != 128 {
        return Err(Error::Gpt);
    }
    read_sector(entries_lba, sector.as_mut_ptr(), high_capacity)?;
    // GPT entry 2 begins at offset 128 of the partition entry array.
    let first = (le_u32(&sector, 128 + 36) as u64) << 32 | le_u32(&sector, 128 + 32) as u64;
    let last = (le_u32(&sector, 128 + 44) as u64) << 32 | le_u32(&sector, 128 + 40) as u64;
    if first == 0
        || first > (u32::MAX - (FIRMWARE_SECTORS - 1)) as u64
        || last < first + (FIRMWARE_SECTORS - 1) as u64
    {
        return Err(Error::Partition);
    }
    for index in (0..FIRMWARE_SECTORS).step_by(READ_CHUNK_SECTORS as usize) {
        let target = (0x4000_0000usize + index as usize * 512) as *mut u32;
        read_blocks(
            first as u32 + index,
            READ_CHUNK_SECTORS,
            target,
            high_capacity,
        )?;
    }
    Ok((FIRMWARE_SECTORS * 512) as usize)
}
