/// One complete Ethernet frame, without FCS. The caller owns the DMA mapping.
/// Keep the buffer valid and untouched until completion.
#[derive(Clone, Copy, Debug)]
pub struct TxTask {
    pub phys: u32,
    pub len: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxError {
    NotReady,
    InvalidBuffer,
    Failed,
    Descriptor,
    /// DMA did not release ownership. The buffer must remain reserved and
    /// untouched until the hardware has been reset (currently until reboot).
    BufferUnavailable,
}

pub type TxDone = Result<(), TxError>;

pub fn valid(task: TxTask) -> bool {
    (14..=1514).contains(&task.len) && task.phys.checked_add(u32::from(task.len) - 1).is_some()
}
