// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! VisionFive 2 v1.3B YT8531/DWMAC raw Ethernet bring-up.
//! See docs/visionfive2/transmit-test.md for the boot contract and frame format.

use super::scheduler::{Shared, DriverState};

mod registers;

mod rx;
pub use self::rx::{RxTask, RxDone, RxError};

mod tx;
pub use self::tx::{TxTask, TxDone, TxError};

mod state;
pub use self::state::State;
