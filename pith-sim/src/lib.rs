//! pith-sim — reusable sim-racing telemetry data sources.
//!
//! Turns raw bytes from a sim into a normalized [`pith_core::simhub::Telemetry`]:
//! - **passive UDP decoders** (Forza, F1/Codemasters, Project CARS/AMS2,
//!   OutGauge, PiBoSo family) behind the [`decoders`] registry,
//! - **active connector protocols** (ACC broadcasting, Assetto Corsa, GT7),
//! - **shared-memory parsers** (rF2/LMU, AC/ACC, RaceRoom) in [`shm`].
//!
//! Depends only on `pith_core` for the shared `Telemetry`, byte readers and wire
//! format — so it can be reused outside this project.

pub mod ac;
pub mod acc;
pub mod codemasters;
pub mod decoders;
pub mod f1;
pub mod forza;
pub mod gt7;
pub mod le;
pub mod outgauge;
pub mod pcars;
pub mod piboso;
pub mod rf2;
pub mod shm;
pub mod shm_read;

pub use decoders::{supported_games, try_decode, Decoded, GameDecoder, REGISTRY};
pub use shm_read::{read_once, ShmRead};
