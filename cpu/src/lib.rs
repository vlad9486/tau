#![no_std]

pub use tau_regs::*;

#[cfg(feature = "cortex-a")]
pub use tau_cortex_a as cortex_a;

#[cfg(feature = "macros")]
pub use tau_cpu_macros::*;

#[cfg(all(test, feature = "macros"))]
mod tests {
    use super::RegisterSetField;

    #[derive(RegisterSetField)]
    #[field_enum(FieldEnumSimple = 12..21)]
    #[field_enum(FieldEnumOverflow = 0..64)]
    #[field_bool(FieldBool = 6)]
    struct Foo(u64);

    enum FieldEnumSimple {
        A = 0x321,
    }

    enum FieldEnumOverflow {
        A = 0,
    }

    struct FieldBool(bool);

    #[test]
    fn enum_simple() {
        let Foo(v) = Foo(0x123456789).set_field(FieldEnumSimple::A);
        assert_eq!(v, 0x123721789);
    }

    #[test]
    fn enum_overflow() {
        let Foo(v) = Foo(0x123456789).set_field(FieldEnumOverflow::A);
        assert_eq!(v, 0);
    }

    #[test]
    fn bool_simple() {
        let Foo(v) = Foo(0x123456789).set_field(FieldBool(true));
        assert_eq!(v, 0x1234567c9);
    }
}
