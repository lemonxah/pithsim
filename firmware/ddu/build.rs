// Build script for the pithddu firmware (Rust / esp-idf-sys).
//
// Just emits the esp-idf-sys link flags. The telemetry field-registry codegen
// lives in the pith-core crate's build.rs (generated from main/field_registry.json).

fn main() {
    embuild::espidf::sysenv::output();
}
