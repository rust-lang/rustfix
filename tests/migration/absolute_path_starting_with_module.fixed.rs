#![allow(unused)]
#![feature(crate_in_paths)]
#![warn(absolute_path_starting_with_module)]
// #![warn(rust_2018_migration)]

mod foo {
    use crate::bar::Bar;
}

pub mod bar {
    pub struct Bar;
}

fn main() {}
