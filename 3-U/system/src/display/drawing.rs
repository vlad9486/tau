// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

/// Colors are 0x00RRGGBB; scanout stores opaque 0xffRRGGBB (B,G,R,A in RAM).
#[derive(Clone, Copy, Debug)]
pub enum Task {
    TestPattern,
    Clear {
        color: u32,
    },
    FillRect {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        color: u32,
    },
}

impl Task {
    /// Clip before indexing RAM. A task is rendered in bounded row batches.
    pub(super) fn rows(self, height: usize) -> core::ops::Range<usize> {
        match self {
            Self::FillRect { y, height: h, .. } => {
                usize::from(y).min(height)..(usize::from(y) + usize::from(h)).min(height)
            }
            _ => 0..height,
        }
    }

    pub(super) fn row(
        self,
        width: usize,
        height: usize,
        row: usize,
        mut put: impl FnMut(usize, u32),
    ) {
        if row >= height || width == 0 {
            return;
        }
        match self {
            Self::Clear { color } => {
                for x in 0..width {
                    put(x, opaque(color));
                }
            }
            Self::FillRect {
                x,
                y,
                width: w,
                height: h,
                color,
            } => {
                if row < usize::from(y) || row >= usize::from(y) + usize::from(h) {
                    return;
                }
                for x in usize::from(x).min(width)..(usize::from(x) + usize::from(w)).min(width) {
                    put(x, opaque(color));
                }
            }
            Self::TestPattern => {
                const BARS: [u32; 8] = [
                    0xffffff, 0xffff00, 0x00ffff, 0x00ff00, 0xff00ff, 0xff0000, 0x0000ff, 0x000000,
                ];
                for x in 0..width {
                    let color = if row < height * 3 / 4 {
                        BARS[x * BARS.len() / width]
                    } else {
                        let red = x * 255 / width.saturating_sub(1).max(1);
                        let green = row * 255 / height.saturating_sub(1).max(1);
                        let blue = if ((x / 32) ^ (row / 32)) & 1 == 0 {
                            0
                        } else {
                            255
                        };
                        ((red as u32) << 16) | ((green as u32) << 8) | blue
                    };
                    put(x, opaque(color));
                }
            }
        }
    }
}

const fn opaque(color: u32) -> u32 {
    0xff00_0000 | (color & 0x00ff_ffff)
}
