# Pith Active Pedals — design & port plan

Source of truth for porting [ChrGri/DIY-Sim-Racing-FFB-Pedal](https://github.com/ChrGri/DIY-Sim-Racing-FFB-Pedal)
(Arduino/ESP-IDF C++ firmware + a C# SimHub plugin) into the pith framework:
Rust firmware under `firmware/pedals`, a shared `pith-pedals-core` protocol/
effects crate, and a dashboard "Pedals" page that replaces the SimHub plugin
end to end (profiles, force-curve editor, live effects) fed by our own
telemetry pipeline (`pith-sim` UDP/shm decoders → the same merge the DDU and
handbrake already use), not SimHub.

**This is a real-time force-control system that pushes back against a human
foot.** Anything that computes motor drive/servo targets gets a phased,
bench-validated rollout (see "Phasing" below) — it is not something to
one-shot from a source read, and this doc says explicitly which parts are
safe to port mechanically vs. which need hardware-in-the-loop validation by
the user before they touch a real actuator.

## 1. What the reference project actually is

Bigger than a firmware+plugin pair — it's two ESP32 targets, a shared C
protocol library, and a WPF SimHub plugin:

- **`ESP32/`** — the pedal controller itself (ESP32-S2, since it does
  `USB_JOYSTICK` HID directly). Owns: ADS1220 loadcell ADC, either a stepper
  (FastAccelStepper) or an iSV57 servo (Modbus RTU) actuator, admittance
  (impedance) force control, ABS/RPM/bite-point/G/wheel-slip/road-impact/
  custom-vibration effect oscillators, USB HID joystick output, USB CDC +
  ESP-NOW wireless command/telemetry channel, EEPROM config persistence,
  OTA (both a pull-URL updater and PlatformIO-upload style).
- **`ESP32_master/`** — an optional bridge board: ESP-NOW ↔ USB CDC, so up to
  3 pedals can each be a cheap wireless node reporting through one master's
  USB port instead of each pedal needing its own USB cable to the PC.
- **`Common_Libs/DiyActivePedal_types`** — the wire-protocol headers, shared
  verbatim between the two firmware targets (this is the actual source of
  truth for the protocol, not the C# `VariablesStruct/` mirror, which lags
  behind it — see field-name drift noted in §2).
- **`SimHubPlugin/`** — WPF plugin: HID discovery + serial-equivalent comms
  (`HidService.cs`), a profile system, a cubic-spline force-curve editor UI,
  and telemetry → `payloadPedalAction` glue (`DIYFFBPedal.cs`, ~1860 lines).

### Control architecture (why the split matters for the port)

The effect **waveforms are generated on the firmware**, not the PC. The PC
side only detects trigger conditions from telemetry and sends small scalar
"action" packets (`PayloadPedalAction_t` — a trigger bit + a magnitude byte
per effect); the firmware's oscillator classes (`ABSOscillation`,
`RPMOscillation`, `GForceEffect`, `WSOscillation`, `RoadImpactEffect`,
`CustomVibration` ×4, all in `ESP32/include/ABSOscillation.h`) own the actual
sine/sawtooth generation, amplitude/frequency shaping, and decay-to-zero
logic, reading their tunables (frequency, amplitude, pattern) from the
one-time `PayloadPedalConfig_t`. This is good news for the port: it means
**our dashboard's job is exactly what SimHub's plugin does** — watch the
telemetry merge, detect edges/magnitudes, and stream 14-byte action packets
— while the actual real-time waveform synthesis lives in the firmware next
to the actuator, at whatever rate the firmware's control loop runs (not
bottlenecked by USB/serial latency to the PC).

The **position/force control loop** (admittance/impedance control — an
academic technique, code references Landi et al.) is the part that actually
moves the motor: `ESP32/include/StepperMovementStrategy.h` (1066 lines) is a
virtual mass-spring-damper simulation solved against the measured loadcell
force each cycle, with an "energy tank" stability monitor, oscillation
detection, Kalman-filtered force estimation, and full pedal kinematics
(stepper position → lever angle → pedal travel, accounting for a 4-bar-ish
`lengthPedalA/B/C/D` linkage geometry). `ESP32/src/isv57communication.cpp`
is a from-scratch Modbus RTU driver for the alternative iSV57 servo actuator.
This is genuinely substantial control-systems engineering, not boilerplate.

### Wire protocol (verbatim, from `Common_Libs/DiyActivePedal_types/src/*.h`)

Frame = `PayloadHeader_t` (6B) + one payload struct + `PayloadFooter_t` (4B),
all `__attribute__((packed))`, little-endian (Xtensa/ESP32 native).

```c
// PedalDefine.h
#define SOF_BYTE_0_U8 0xAAU
#define SOF_BYTE_1_U8 0x55U
#define EOF_BYTE_0_U8 0xAAU
#define EOF_BYTE_1_U8 0x56U

// PayloadHeader.h
typedef struct __attribute__((packed)) PayloadHeader {
  uint8_t startOfFrame0_u8;   // SOF_BYTE_0_U8
  uint8_t startOfFrame1_u8;   // SOF_BYTE_1_U8
  uint8_t payloadType_u8;     // see payload type IDs below
  uint8_t version_u8;         // must match receiver's compiled version (172)
  uint8_t storeToEeprom_u8;   // config packets only: persist after apply
  uint8_t pedalTag_u8;        // which pedal (clutch/brake/throttle) — see PedalIdEnum
} PayloadHeader_t;

// PayloadFooter.h
typedef struct __attribute__((packed)) PayloadFooter {
  uint16_t checkSum_u16;      // Fletcher-16 over header+payload (not footer)
  uint8_t enfOfFrame0_u8;     // EOF_BYTE_0_U8
  uint8_t enfOfFrame1_u8;     // EOF_BYTE_1_U8
} PayloadFooter_t;
```

Checksum (`ESP32/src/Main.cpp:136`, inline Fletcher-16 over
`sizeof(header) + sizeof(payload)` bytes, footer excluded):

```c
inline uint16_t checksumCalculator_u16(uint8_t *data_pu8, uint16_t length_u16) {
  uint8_t sum1_u8 = 0, sum2_u8 = 0;
  for (int i = 0; i < length_u16; i++) {
    sum1_u8 = (sum1_u8 + data_pu8[i]) % 255;
    sum2_u8 = (sum2_u8 + sum1_u8) % 255;
  }
  return (sum2_u8 << 8) | sum1_u8;
}
```

Payload type IDs (`SimHubPlugin/VariablesStruct/constants.cs`; version 172):

| type | value | struct |
|---|---|---|
| config | 100 | `PayloadPedalConfig_t` |
| action | 110 | `PayloadPedalAction_t` |
| state (basic) | 120 | `PayloadPedalStateBasic_t` |
| state (extended) | 130 | `PayloadPedalStateExtended_t` |
| servo config | 170 | `payloadServoConfig_t` |
| bridge state | 210 | `PayloadBridgeState_t` |
| OTA | 220 | `PayloadOtaInfo_t` |
| HID message | 225 | `PayloadHidMessage_t` |

`payloadPedalAction_st` (14 bytes, real-time — this is what our dashboard's
effects engine sends, replacing SimHub's plugin output):

```c
typedef struct __attribute__((packed)) PayloadPedalAction {
  uint8_t triggerAbs_u8;
  uint8_t systemAction_u8;         // 1=reset pos, 2=restart ESP, 3=OTA enable, 4=pairing
  uint8_t startSystemIdentification_u8;
  uint8_t returnPedalConfig_u8;
  uint8_t rpm_u8;
  uint8_t gValue_u8;
  uint8_t wheelSlip_u8;
  uint8_t impactValue_u8;
  uint8_t triggerCv1_u8;
  uint8_t triggerCv2_u8;
  uint8_t triggerCv3_u8;
  uint8_t triggerCv4_u8;
  uint8_t rudderAction_u8;
  uint8_t rudderBrakeAction_u8;
} PayloadPedalAction_t;
```

`payloadPedalConfig_st` — one-time config, ~140 bytes, the tunables behind
every effect + the 11-point force/travel curve + geometry + calibration.
Full field list ported verbatim into `pith-pedals-core::protocol::PedalConfig`
(see that module — reproducing all ~90 fields here would just duplicate the
code; the header is `Common_Libs/DiyActivePedal_types/src/PayloadPedalConfig.h`
in the reference clone). Notable groups: `relativeForce00..10_u8` +
`relativeTravel00..10_u8` (the force curve, 11 points, 0-100% each axis),
`absFrequency/absAmplitude/absPattern/absForceOrTarvelBit`, `rpmMaxFreq/
rpmMinFreq/rpmAmp`, `bpTriggerValue/bpAmp/bpFreq/bpTrigger` (bite point),
`gMulti/gWindow`, `wsAmp/wsFreq`, `roadMulti/roadWindow`, `cvAmp1-4/cvFreq1-4`
(4 custom vibration slots), `virtualPedalMassInPercent/
virtualPedalDampingInPercent`, `endstopStiffness_kg_mm/endstopTravelRange_mm`,
`lengthPedalA/B/D/CHorizontal/CVertical/Travel` (mm, pedal linkage geometry).

**Important field-name drift**: the C# `SimHubPlugin/VariablesStruct/*.cs`
mirrors (which I read first) use an older naming convention
(`lengthPedal_b`, `virtualPedalMass_u8`) than the current C headers in
`Common_Libs` (`lengthPedalB_i16`, `virtualPedalMassInPercent_u8`) — same
fields, same byte order, just renamed at some point without the C# side
being fully resynced. **The C header is ground truth for the port**; the C#
names are informative only for cross-referencing plugin logic.

## 2. What we're building instead of SimHub

| SimHub plugin piece | Pith replacement |
|---|---|
| `HidService.cs` (USB HID discovery + framing) | `pith_device::Pedals` (mirrors `Handbrake`/`Dash`) in `pith-device` |
| `DIYFFBPedal.cs` telemetry → effect triggers | dashboard effects engine, fed by the existing `pith-sim` UDP/shm merge (same telemetry the DDU/race-screen use) instead of SimHub's property bag |
| `CubicSpline.cs` + curve editor UI | a Pedals-page force-curve editor in the dashboard (Slint), talking `pith-pedals-core::curve` |
| Profile system (per-game/per-car JSON) | reuses the dashboard's existing car-library/profile machinery (same pattern as shift-light profiles) |
| `EspFlasher.cs` (OTA/PlatformIO upload) | the existing `@OTA` dual-slot protocol (already shipped for DDU + handbrake) |
| ESP-NOW pairing UI | out of scope for v1 — v1 targets one pedal set, one USB (or WiFi, see §4) link per pedal; the wireless bridge is a later addition if the wired story works well |

## 3. Phasing (why this isn't a one-session port)

**Phase 1 — protocol + effects-trigger engine + dashboard UI (this pass).**
Mechanically portable, verifiable without hardware, no motor safety
implications:
- `pith-pedals-core`: wire structs, Fletcher-16 checksum, SOF/EOF framing,
  the 11-point force curve + its interpolation, and the effect-*trigger*
  logic (the "detect ABS edge, compute G magnitude byte" side — not the
  waveform generator, which stays firmware-side per §1).
- `firmware/pedals` skeleton: boots, exposes the report-id-2 command
  channel + `@CAP`/`@OTA` (copy the handbrake's proven USB shim), receives
  and stores `PedalConfig`, echoes `PedalState`. **No motor/servo driver
  yet** — the actuator output stays a documented stub until Phase 2.
- Dashboard: `pith_device::Pedals` transport, a Pedals page (force-curve
  editor, effect gain sliders, profiles), and the effects-trigger engine
  wired to the telemetry merge.
- New telemetry fields the effects need that aren't decoded yet (lateral/
  longitudinal G, per-wheel slip ratio, suspension travel) get *added to the
  schema* now; wiring real per-game shared-memory offsets for them is
  deferred until verified against each game's official SDK header (AC/ACC's
  `SPageFilePhysics.accG`/`wheelSlip` offsets specifically — **not guessed**,
  since a wrong offset here would silently feed garbage into a physical
  actuator's force target).

**Phase 2 — actuator drive (needs your bench, one step at a time).** The
admittance controller (`StepperMovementStrategy.h`), the iSV57 Modbus driver,
and the loadcell ADC path get ported function-by-function against the C++
source, but every change ships as "compiles, does NOT enable torque" until
you've bench-tested it with the actuator on a fixture (not your foot) and
confirmed behavior matches the reference firmware. This phase also needs
you to pin down which actuator you're actually building (stepper+leadscrew
vs. iSV57 servo) since the drivers are unrelated code paths.

**Phase 3 — wireless (ESP-NOW multi-pedal bridge)**, if the wired v1 proves
out and you still want it — see §4 for the *WiFi* (not ESP-NOW) transport,
which is a separate, simpler ask already in progress for all Pith devices.

## 4. WiFi transport for all Pith devices (DDU, handbrake, pedals)

Requested for every Pith firmware, not just pedals. The reference project's
own wireless story is ESP-NOW (a connectionless 2.4 GHz protocol, no router/
IP stack, used purely for the pedal↔bridge hop) — not WiFi/TCP. For pith,
plain WiFi (station mode, TCP) is the better fit: it reaches the dashboard
directly (no extra bridge board), reuses the exact same line protocol
already spoken on HID report id 2 (the `@`-command channel + `$`/status
replies), and needs no new wire format — just a second transport carrying
the same bytes. Design:

- **Device side**: `esp_wifi` station mode, credentials provisioned via a
  `@WIFI<ssid>,<pass>` command over USB the first time (or a captive-portal
  AP mode as a fallback — TBD), then a TCP listener on a fixed port speaking
  the identical framed lines the HID channel does. mDNS (`_pith._tcp`)
  advertises it so the dashboard doesn't need a hardcoded IP.
- **Host side**: `pith-device` gets a `Tcp` transport implementing the same
  interface as `Hid`/`Serial` (`open`/`write`/`read_line`/`drain`), and a
  discovery step (mDNS browse, falling back to a manual IP field) alongside
  the existing HID VID/PID scan.
- **OTA over WiFi**: the existing `@OTA` protocol is transport-agnostic
  already (it's just bytes on whatever channel `write_line`/`feed` are
  hooked to) — once the TCP transport exists, OTA over WiFi is close to
  free.
- Applies to DDU, handbrake, and pedals identically once built once in
  `pith-device` + one shared firmware-side module — this becomes a
  `firmware/<device>/src/wifi.rs` slice per device rather than three
  separate implementations.

This is tracked as its own follow-on (task: "WiFi transport for all Pith
firmware") — building it against three device targets at once is its own
scoped pass, done after the Phase 1 pedals plumbing above lands so there's
a third real consumer to validate the transport trait against, not just
two.

## 5. PID allocation

Adds to `pith-device`'s table (`pith-device/src/lib.rs`):

```rust
pub const PID_PEDALS: u16 = 0x8002;
```
