//! Telemetry field registry — the table, `FieldDef`, `FIELD_COUNT`, and lookups
//! come from `pith-core` (the single source of truth, generated from the firmware's
//! field_registry.json). Only the `FIELD_<NAME>` id constants are generated here,
//! for ergonomic `telem[FIELD_X]` indexing.

pub use pith_core::format::Fmt;
pub use pith_core::registry::{field_def, field_id_from_str, FIELDS, FIELD_COUNT};

// FIELD_NONE + FIELD_<NAME> id constants (1-based; 0 = none). Several are
// device-only fields the dashboard doesn't index, hence allow-dead.
#[allow(dead_code)]
mod ids {
    include!(concat!(env!("OUT_DIR"), "/field_ids.rs"));
}
pub use ids::*;
