// Generates the telemetry field registry from main/field_registry.json — the
// single source of truth shared (by path) with the pithddu-dashboard app. Emits
// `$OUT_DIR/field_registry.rs`, included by src/lib.rs. Replaces the device side
// of tools/gen_field_registry.py.
//
// Field id = array index + 1 (0 = none), matching the C generator and the wire
// order used by the `@T` reply and the SimHub `$` frame.

use std::path::Path;

fn main() {
    // Single source of truth, shared by path with the dashboard. Lives in the
    // firmware tree; pith-core sits at the monorepo root.
    let json_path = "../firmware/ddu/main/field_registry.json";
    println!("cargo:rerun-if-changed={json_path}");

    let raw =
        std::fs::read_to_string(json_path).unwrap_or_else(|e| panic!("read {json_path}: {e}"));
    let doc: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {json_path}: {e}"));
    let fields = doc["fields"].as_array().expect("`fields` array");

    let mut out = String::new();
    out.push_str("// @generated from main/field_registry.json by build.rs — do not edit.\n\n");
    out.push_str("pub struct FieldDef {\n");
    out.push_str("    pub name: &'static str,\n");
    out.push_str("    pub fmt: Fmt,\n");
    out.push_str("    pub scale: i32,\n");
    out.push_str("    pub label: &'static str,\n");
    out.push_str("}\n\n");

    out.push_str("/// All bindable fields, indexed by (id - 1).\n");
    out.push_str("pub const FIELDS: &[FieldDef] = &[\n");
    for f in fields {
        let name = f["name"].as_str().expect("field.name");
        let fmt = f["fmt"].as_str().unwrap_or("int");
        let sc = f["sc"].as_i64().unwrap_or(1);
        let label = f["label"].as_str().unwrap_or("");
        let fmt_variant = fmt_to_variant(fmt, name);
        out.push_str(&format!(
            "    FieldDef {{ name: {name:?}, fmt: Fmt::{fmt_variant}, scale: {sc}, label: {label:?} }},\n"
        ));
    }
    out.push_str("];\n\n");

    // FIELD_COUNT mirrors the C constant: number of fields + 1 (the none slot).
    out.push_str(&format!(
        "/// Number of field ids including the 0 = none slot.\npub const FIELD_COUNT: usize = {};\n\n",
        fields.len() + 1
    ));

    // field_value: map a 1-based field id to its raw telemetry value.
    out.push_str("/// Raw value of field `id` (1-based) from `t`; 0 for none / out of range.\n");
    out.push_str("pub fn field_value(t: &Telemetry, id: usize) -> i32 {\n");
    out.push_str("    match id {\n");
    for (idx, f) in fields.iter().enumerate() {
        let accessor = f["accessor"].as_str().expect("field.accessor");
        let field = accessor
            .strip_prefix("t->")
            .unwrap_or_else(|| panic!("accessor `{accessor}` must start with t->"));
        out.push_str(&format!("        {} => t.{},\n", idx + 1, field));
    }
    out.push_str("        _ => 0,\n");
    out.push_str("    }\n}\n\n");

    // set_field: inverse of field_value — write a raw value into the field id's
    // struct slot. Lets a flat field array (e.g. the dashboard's telem[]) be
    // rehydrated into a Telemetry for the shared renderer.
    out.push_str(
        "/// Write raw value `v` into field `id` (1-based); no-op for none / out of range.\n",
    );
    out.push_str("pub fn set_field(t: &mut Telemetry, id: usize, v: i32) {\n");
    out.push_str("    match id {\n");
    for (idx, f) in fields.iter().enumerate() {
        let accessor = f["accessor"].as_str().expect("field.accessor");
        let field = accessor.strip_prefix("t->").unwrap();
        out.push_str(&format!("        {} => t.{} = v,\n", idx + 1, field));
    }
    out.push_str("        _ => {}\n");
    out.push_str("    }\n}\n\n");

    // telemetry_from_fields: rebuild a Telemetry from a flat array indexed by
    // field id (index 0 = none). gear is not a numeric field, so set it after.
    out.push_str("/// Rebuild a Telemetry from a flat field array (index = field id, 0 = none).\n");
    out.push_str("pub fn telemetry_from_fields(fields: &[i32]) -> Telemetry {\n");
    out.push_str("    let mut t = Telemetry::default();\n");
    out.push_str("    let mut id = 1;\n");
    out.push_str("    while id < fields.len() {\n");
    out.push_str("        set_field(&mut t, id, fields[id]);\n");
    out.push_str("        id += 1;\n");
    out.push_str("    }\n");
    out.push_str("    t\n}\n");

    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("field_registry.rs");
    std::fs::write(&dest, out).expect("write field_registry.rs");
}

fn fmt_to_variant(fmt: &str, name: &str) -> &'static str {
    match fmt {
        "int" => "Int",
        "fixed1" => "Fixed1",
        "fixed2" => "Fixed2",
        "time" => "Time",
        "sector" => "Sector",
        "delta" => "Delta",
        "string" => "Str",
        other => panic!("unknown fmt `{other}` for field `{name}`"),
    }
}
