#[macro_export]
macro_rules! csrr {
    ($r:expr) => {{
        let v: usize;

        #[allow(unused_unsafe)]
        unsafe {
            core::arch::asm!(concat!("csrr {0}, ", $r), out(reg) v, options(nomem, nostack));
        }

        v
    }};
}
pub use csrr;

#[macro_export]
macro_rules! csrw {
    ($r:expr, $v:expr) => {{
        let v = $v;
        unsafe {
            core::arch::asm!(concat!("csrw ", $r, ", {0}"), in(reg) v, options(nomem, nostack));
        }
    }};
}
pub use csrw;

#[macro_export]
macro_rules! csrrs {
    ($r:expr, $v:expr) => {{
        let v = $v;
        unsafe {
            core::arch::asm! {
                concat!("csrrs {tmp}, ", $r, ", {mask}"),
                mask = in(reg) v,
                tmp = out(reg) _,
                options(nomem, nostack)
            }
        }
    }};
}
pub use csrrs;

#[macro_export]
macro_rules! csrrc {
    ($r:expr, $v:expr) => {{
        unsafe {
            core::arch::asm! {
                concat!("csrrc {tmp}, ", $r, ", {mask}"),
                mask = in(reg) $v,
                tmp = out(reg) _,
                options(nomem, nostack)
            }
        }
    }};
}
pub use csrrc;
