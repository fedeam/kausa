//! The targets include kausa's modules by path, and src/prompt.rs leaves out, under
//! `cfg(fuzzing)`, what needs a module of kausa's that this crate does not have. `cargo fuzz
//! build` sets that cfg; this script sets it too, so that `cargo check` or `cargo build` of this
//! crate compiles the same code.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(fuzzing)");
    println!("cargo::rustc-cfg=fuzzing");
}
