// Build script for the pith-hb firmware (Rust / esp-idf-sys). Just emits the
// esp-idf-sys link flags.

fn main() {
    embuild::espidf::sysenv::output();
}
