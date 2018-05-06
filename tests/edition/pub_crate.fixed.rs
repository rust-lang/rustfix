#![allow(unused)]
#[warn(rust_2018_migration)]

mod private_mod {
    crate const FOO: &str = "BAR";
}

fn main() {}
