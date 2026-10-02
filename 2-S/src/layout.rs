// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

pub struct StaticModule {
    pub name: [u8; 20],
    pub offset: usize,
    pub pages: usize,
    pub user: bool,
}

include!(concat!(env!("OUT_DIR"), "/modules.rs"));
