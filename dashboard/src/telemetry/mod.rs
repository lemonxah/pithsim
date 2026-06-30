//! Dashboard-local telemetry glue. The game decoders, connector protocols and
//! shared-memory parsers now live in the reusable `pith-sim` crate; what's left
//! here is app-specific: derived fields, the `$`-frame serializer, formatting
//! helpers, the field-id registry, and the `/dev/shm` reader.
pub mod derive;
pub mod field_registry;
pub mod format;
pub mod le;
pub mod serialize;

pub use field_registry::*;
pub use format::*;
pub use serialize::frame_from_telem;
