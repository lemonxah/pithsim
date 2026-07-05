//! Shared data model + math for the Pith active pedal (firmware + dashboard).
//! See `docs/pedals.md` in the repo root for the full port plan and what's
//! ported vs. deferred; briefly: the wire *data model* (config/action/state
//! fields, the force curve) is a faithful port of
//! github.com/ChrGri/DIY-Sim-Racing-FFB-Pedal, encoded pith's own way
//! (JSON over the `@`-command channel, like the DDU) rather than that
//! project's byte-packed/checksummed framing.

pub mod admittance;
pub mod controller;
pub mod curve;
pub mod effects;
pub mod filter;
pub mod kinematics;
pub mod loadcell;
pub mod modbus;
pub mod protocol;
pub mod servo_jss57p;
