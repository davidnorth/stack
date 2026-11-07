//! Command-line entry point for `focus-stack`.
//!
//! Argument parsing and the pipeline wiring arrive in later sessions. For now
//! the binary only reports the crate version so the scaffold is runnable.

fn main() {
    println!("focus-stack {}", focus_stack::VERSION);
}
