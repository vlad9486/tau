//! Single-buffer receive contract and DWMAC4/5 write-back validation.
pub const BUFFER_SIZE: u16 = 2048;

/// Caller-owned, DMA-accessible memory. Do not access it until completion.
#[derive(Clone, Copy, Debug)]
pub struct RxTask {
    pub phys: u32,
    pub capacity: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxError {
    InvalidBuffer,
    Failed,
    Descriptor,
    Fragmented,
    InvalidLength,
    /// Keep the buffer reserved and untouched until reboot.
    BufferUnavailable,
}

/// Length excludes FCS; the MAC is configured to retain CRC and padding.
pub type RxDone = Result<u16, RxError>;

pub fn valid(task: RxTask) -> bool {
    task.capacity >= BUFFER_SIZE
        && task.phys.is_multiple_of(4)
        && task.phys.checked_add(u32::from(BUFFER_SIZE) - 1).is_some()
}

/// None means the descriptor still belongs to hardware.
pub fn completion(status: u32) -> Option<RxDone> {
    if status & (1 << 31) != 0 {
        return None;
    }
    Some(if status & ((1 << 30) | (1 << 15)) != 0 {
        Err(RxError::Descriptor)
    } else if status & (3 << 28) != 3 << 28 {
        Err(RxError::Fragmented)
    } else {
        let len = (status & 0x7fff) as u16;
        if !(64..=1518).contains(&len) {
            Err(RxError::InvalidLength)
        } else {
            Ok(len - 4)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_and_frame_boundaries() {
        assert_eq!(completion(u32::MAX), None);
        assert_eq!(completion((3 << 28) | 64), Some(Ok(60)));
        assert_eq!(completion((3 << 28) | 1518), Some(Ok(1514)));
        for status in [0, 1 << 28, 1 << 29] {
            assert_eq!(completion(status | 64), Some(Err(RxError::Fragmented)));
        }
    }

    #[test]
    fn reject_errors_context_and_bad_lengths() {
        for flag in [1 << 15, 1 << 30] {
            assert_eq!(
                completion((3 << 28) | flag | 64),
                Some(Err(RxError::Descriptor))
            );
        }
        for len in [0, 4, 63, 1519, 2048, 32767] {
            assert_eq!(
                completion((3 << 28) | len),
                Some(Err(RxError::InvalidLength))
            );
        }
    }

    #[test]
    fn buffer_bounds() {
        assert!(valid(RxTask {
            phys: 0x70006000,
            capacity: 2048
        }));
        assert!(valid(RxTask {
            phys: 0xfffff800,
            capacity: 2048
        }));
        for (phys, capacity) in [(0x70006000, 2047), (0x70006001, 2048), (0xfffffffc, 2048)] {
            assert!(!valid(RxTask { phys, capacity }));
        }
    }
}
