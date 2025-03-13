#![no_std]
#![feature(custom_test_frameworks)]
#![test_runner(tau::tester::test_runner)]
#![feature(strict_provenance_lints)]
#![warn(fuzzy_provenance_casts)]

mod asm;

pub mod cpu;

pub mod sbi;

pub mod llfree;

pub mod vmem;

pub mod module;

pub mod scheduler;

pub mod state;
