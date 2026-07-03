// Build script for the pith-pedals firmware (Rust / esp-idf-sys). Just emits the
// esp-idf-sys link flags.

fn main() {
    embuild::espidf::sysenv::output();
}
