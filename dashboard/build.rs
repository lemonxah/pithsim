use std::path::Path;

fn main() {
    let cfg = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/app.slint", cfg).expect("compile ui/app.slint");

    gen_field_registry();
}

// Only the FIELD_<NAME> id constants are generated here (for ergonomic telem
// indexing). The registry TABLE, FieldDef, FIELD_COUNT and lookups come from
// pith-core — one source of truth, generated from the same field_registry.json.
fn gen_field_registry() {
    let json_path = Path::new("../firmware/ddu/main/field_registry.json");
    println!("cargo:rerun-if-changed={}", json_path.display());
    let raw = std::fs::read_to_string(json_path)
        .expect("read ../firmware/ddu/main/field_registry.json");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse field_registry.json");
    let fields = parsed["fields"].as_array().expect("fields array");

    let mut out = String::new();
    out.push_str("// AUTO-GENERATED from firmware/main/field_registry.json by build.rs.\n");
    out.push_str("pub const FIELD_NONE: usize = 0;\n");
    for (i, fd) in fields.iter().enumerate() {
        let name = fd["name"].as_str().unwrap().to_uppercase();
        out.push_str(&format!("pub const FIELD_{name}: usize = {};\n", i + 1));
    }

    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("field_ids.rs");
    std::fs::write(&dest, out).expect("write field_ids.rs");
}
