//! Typed, zero-copy readers for the rF2 / LMU shared-memory buffers — the single
//! source of truth for byte offsets. Mirrors TheIronWolf's `rF2State.h` (itself a
//! mirror of ISI's `InternalsPlugin.hpp`), `#pragma pack(4)`. Reading a value is a
//! named accessor, not a hand-counted offset; offsets live here once and are
//! validated against the SDK. This replaces the scattered `le::f64(telem, base+N)`
//! literals in `shm.rs` so adding a field means adding ONE accessor here.
//!
//! Layout notes (vehicle telemetry, `rF2VehicleTelemetry`, stride 1888):
//!   the 4-wheel array (`rF2Wheel`, 260 bytes each) is the LAST member at +848,
//!   so all scalar fields live in 0..848.

use pith_core::le;

/// One vehicle's telemetry block (`rF2VehicleTelemetry`, 1888 bytes). Holds the
/// whole buffer + the element base so accessors stay simple; bounds are checked
/// once in [`VehicleTelem::at`].
pub struct VehicleTelem<'a> {
    b: &'a [u8],
    base: usize,
}

/// Vehicle-relative field offsets (bytes from the element base).
pub mod telem_off {
    pub const ID: usize = 0; // mID (long)
    pub const LAP_NUMBER: usize = 20; // mLapNumber (long)
    pub const LAP_START_ET: usize = 24; // mLapStartET (double, s)
    pub const ELAPSED_TIME: usize = 12; // mElapsedTime (double, s)
    pub const LOCAL_VEL: usize = 184; // mLocalVel (Vec3 double, m/s)
    pub const GEAR: usize = 352; // mGear (long, -1=R 0=N)
    pub const ENGINE_RPM: usize = 356; // mEngineRPM (double)
    pub const ENGINE_WATER_TEMP: usize = 364; // mEngineWaterTemp (double, °C)
    pub const ENGINE_OIL_TEMP: usize = 372; // mEngineOilTemp (double, °C)
                                            // Raw driver inputs. Per rF2State.h the block at 388..420 is the
                                            // UNFILTERED set (mUnfilteredThrottle/Brake/Steering/Clutch); the filtered
                                            // set follows at 420..452. These offsets always read the unfiltered ones —
                                            // earlier names said "FILTERED", contradicting the header.
    pub const UNFILTERED_THROTTLE: usize = 388; // mUnfilteredThrottle (double, 0..1)
    pub const UNFILTERED_BRAKE: usize = 396; // mUnfilteredBrake (double, 0..1)
    pub const UNFILTERED_STEERING: usize = 404; // mUnfilteredSteering (double, -1..1)
    pub const UNFILTERED_CLUTCH: usize = 412; // mUnfilteredClutch (double, 0..1)
    pub const FUEL: usize = 524; // mFuel (double, litres)
    pub const MAX_RPM: usize = 532; // mEngineMaxRPM (double)
    pub const HEADLIGHTS: usize = 543; // mHeadlights (u8)
    pub const SPEED_LIMITER: usize = 604; // mSpeedLimiter (u8)
    pub const FUEL_CAPACITY: usize = 608; // mFuelCapacity (double, litres)
    pub const IGNITION_STARTER: usize = 619; // mIgnitionStarter (u8)
    pub const FRONT_COMPOUND_NAME: usize = 620; // mFrontTireCompoundName[18] (ascii)
    pub const REAR_COMPOUND_NAME: usize = 638; // mRearTireCompoundName[18] (ascii)
    pub const BATTERY_CHARGE_FRACTION: usize = 696; // mBatteryChargeFraction (double, 0..1)
    pub const ELECTRIC_BOOST_MOTOR_STATE: usize = 736; // mElectricBoostMotorState (u8)
    pub const WHEELS: usize = 848; // mWheels[4] (rF2Wheel, last member)
    pub const STRIDE: usize = 1888;
}

/// Per-wheel field offsets (bytes from a wheel sub-struct base). `rF2Wheel` = 260.
pub mod wheel_off {
    pub const BRAKE_TEMP: usize = 24; // mBrakeTemp (double, Kelvin)
    pub const PRESSURE: usize = 120; // mPressure (double, kPa)
    pub const TEMPERATURE: usize = 128; // mTemperature[3] (double, Kelvin) L/C/R
    pub const WEAR: usize = 152; // mWear (double, 0..1 remaining)
    pub const CARCASS_TEMP: usize = 204; // mTireCarcassTemperature (double, Kelvin)
    pub const INNER_LAYER_TEMP: usize = 212; // mTireInnerLayerTemperature[3] (double, Kelvin)
    pub const STRIDE: usize = 260;
}

impl<'a> VehicleTelem<'a> {
    /// View the vehicle element at `base`; `None` if the buffer is too short.
    pub fn at(b: &'a [u8], base: usize) -> Option<Self> {
        if b.len() < base + telem_off::STRIDE {
            return None;
        }
        Some(Self { b, base })
    }

    #[inline]
    fn f64(&self, off: usize) -> f64 {
        le::f64(self.b, self.base + off)
    }
    #[inline]
    fn i32(&self, off: usize) -> i32 {
        le::i32(self.b, self.base + off)
    }
    #[inline]
    fn u8(&self, off: usize) -> u8 {
        self.b[self.base + off]
    }

    pub fn id(&self) -> i32 {
        self.i32(telem_off::ID)
    }
    pub fn lap_number(&self) -> i32 {
        self.i32(telem_off::LAP_NUMBER)
    }
    pub fn elapsed_time(&self) -> f64 {
        self.f64(telem_off::ELAPSED_TIME)
    }
    pub fn lap_start_et(&self) -> f64 {
        self.f64(telem_off::LAP_START_ET)
    }
    pub fn gear(&self) -> i32 {
        self.i32(telem_off::GEAR)
    }
    pub fn rpm(&self) -> f64 {
        self.f64(telem_off::ENGINE_RPM)
    }
    pub fn max_rpm(&self) -> f64 {
        self.f64(telem_off::MAX_RPM)
    }
    pub fn water_temp(&self) -> f64 {
        self.f64(telem_off::ENGINE_WATER_TEMP)
    }
    pub fn oil_temp(&self) -> f64 {
        self.f64(telem_off::ENGINE_OIL_TEMP)
    }
    pub fn throttle(&self) -> f64 {
        self.f64(telem_off::UNFILTERED_THROTTLE)
    }
    pub fn brake(&self) -> f64 {
        self.f64(telem_off::UNFILTERED_BRAKE)
    }
    pub fn steering(&self) -> f64 {
        self.f64(telem_off::UNFILTERED_STEERING)
    }
    pub fn clutch(&self) -> f64 {
        self.f64(telem_off::UNFILTERED_CLUTCH)
    }
    pub fn fuel(&self) -> f64 {
        self.f64(telem_off::FUEL)
    }
    pub fn fuel_capacity(&self) -> f64 {
        self.f64(telem_off::FUEL_CAPACITY)
    }
    pub fn headlights(&self) -> bool {
        self.u8(telem_off::HEADLIGHTS) != 0
    }
    pub fn speed_limiter(&self) -> bool {
        self.u8(telem_off::SPEED_LIMITER) != 0
    }
    pub fn ignition(&self) -> bool {
        self.u8(telem_off::IGNITION_STARTER) != 0
    }
    pub fn battery_charge_fraction(&self) -> f64 {
        self.f64(telem_off::BATTERY_CHARGE_FRACTION)
    }
    pub fn electric_boost_motor_state(&self) -> i32 {
        self.u8(telem_off::ELECTRIC_BOOST_MOTOR_STATE) as i32
    }

    /// Speed (m/s) from the local-velocity vector magnitude.
    pub fn speed_ms(&self) -> f64 {
        let o = telem_off::LOCAL_VEL;
        let (vx, vy, vz) = (self.f64(o), self.f64(o + 8), self.f64(o + 16));
        (vx * vx + vy * vy + vz * vz).sqrt()
    }

    /// Front / rear tyre compound name (NUL-terminated ASCII, max 18 bytes).
    pub fn front_compound_name(&self) -> &'a str {
        ascii(self.b, self.base + telem_off::FRONT_COMPOUND_NAME, 18)
    }
    pub fn rear_compound_name(&self) -> &'a str {
        ascii(self.b, self.base + telem_off::REAR_COMPOUND_NAME, 18)
    }

    /// Wheel `i` (0 FL, 1 FR, 2 RL, 3 RR).
    pub fn wheel(&self, i: usize) -> Wheel<'a> {
        Wheel {
            b: self.b,
            base: self.base + telem_off::WHEELS + i * wheel_off::STRIDE,
        }
    }
}

/// One wheel's telemetry (`rF2Wheel`, 260 bytes).
pub struct Wheel<'a> {
    b: &'a [u8],
    base: usize,
}

impl Wheel<'_> {
    #[inline]
    fn f64(&self, off: usize) -> f64 {
        le::f64(self.b, self.base + off)
    }
    /// Surface tread temp, Kelvin: zone 0 inner(L), 1 middle, 2 outer(R). The
    /// outermost (track-contact) layer — runs much hotter than the HUD reading.
    pub fn surface_temp_k(&self, zone: usize) -> f64 {
        self.f64(wheel_off::TEMPERATURE + zone * 8)
    }
    /// Inner-layer rubber temp, Kelvin (zone 0/1/2 = inner/mid/outer). The layer
    /// the in-game tyre HUD shows — between the surface tread and the carcass.
    pub fn inner_temp_k(&self, zone: usize) -> f64 {
        self.f64(wheel_off::INNER_LAYER_TEMP + zone * 8)
    }
    pub fn carcass_temp_k(&self) -> f64 {
        self.f64(wheel_off::CARCASS_TEMP)
    }
    pub fn brake_temp_k(&self) -> f64 {
        self.f64(wheel_off::BRAKE_TEMP)
    }
    pub fn pressure_kpa(&self) -> f64 {
        self.f64(wheel_off::PRESSURE)
    }
    /// Fraction of tread remaining (0..1).
    pub fn wear_fraction(&self) -> f64 {
        self.f64(wheel_off::WEAR)
    }
}

/// NUL-terminated ASCII at `[o, o+max)`, trimmed; empty if out of range.
fn ascii(b: &[u8], o: usize, max: usize) -> &str {
    let end = (o + max).min(b.len());
    if o >= end {
        return "";
    }
    let slice = &b[o..end];
    let n = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
    core::str::from_utf8(&slice[..n]).unwrap_or("").trim()
}
