pub trait Testable {
    fn run(&self);
}

pub fn test_runner(_tests: &[&dyn Testable]) {}
