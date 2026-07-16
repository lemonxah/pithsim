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

## 0. Confirmed target hardware

Building **gilphilbert's "PCBA V2" control board, v2.2 Rev B**
(`gilphilbert/DIY-Sim-Racing-FFB-Pedal-PCBs`, path `v2/v2.2/RevB`) — an
integrated control+power board (single PCB, no separate power board like the
v1 design). Confirmed from that repo:

- **MCU**: ESP32-S3FH4R2 (from the board's BOM CSV) — this is why the pedals
  firmware targets `xtensa-esp32s3-espidf`, matching the DDU's chip.
- **Loadcell ADC**: **ADS1256**, not ADS1220. Traced precisely in the
  reference firmware: this board is `PCB_VERSION == 9` in `ESP32/include/
  Main.h` (`ControlBoard_PCBA_V2X` PlatformIO env), and that `#if` block
  never defines `USES_ADS1220`. `LoadCell.cpp` branches on
  `#ifndef USES_ADS1220` to a genuine `<ADS1256.h>` driver (7.68 MHz
  crystal, 2.5V vref) — so ADS1220 (§1 below) is what the *newer* V6/V7 dev
  boards (PCB_VERSION 13/14) use, not this one.
- **Actuator**: **not the iSV57T** (the reference project's default for this
  board) — this build uses a **JSSmotor/Dewo JSS57P1.5N** (NEMA23 closed-loop
  stepper / "integrated digital hybrid servo", 1000-line encoder, 1.5 N·m
  holding torque, 24-48V). Confirmed on the physical unit: it exposes
  **RS232 plus PEND+/PEND− and ALM+/ALM− outputs** (the PCBA has no
  terminals for the latter — they land on GPIO34/35 pads, see
  `docs/pedals-board-map.md`). Confirmed **not** the sibling `JSS57-R` product
  (that's a different device on an RS485 bus with a completely different
  register map — its manual was fetched first by mistake from an AliExpress
  listing and is *not* applicable here; kept only as a source of
  generic Modbus-RTU CRC/framing test vectors, since that part of the
  protocol is universal). The real reference is the manufacturer's manual
  for this product:
  <https://cdn.shopify.com/s/files/1/0014/4313/5560/files/Nema23_integrated_digital_hybrid_servo.pdf>,
  and a structured protocol brief the user compiled from it.
  **Confirmed**: **Modbus RTU over the drive's RS232 tuning port**,
  parameter number == holding register address (decimal 0-34).
  Full register map + guarded read/write helpers are in
  `pith-pedals-core/src/servo_jss57p.rs` (generic Modbus RTU framing lives
  in `pith-pedals-core/src/modbus.rs`), transcribed from that brief.
  **Board-side reality (2026-07-16)**: the PCBA v2.2b's servo-facing
  interface is only `PUL · DIR · [RX · TX · GND] RS232 · VCC · GND` — the
  BOARD has **no ALM/PEND input terminals** (the reference design leaves
  those drive outputs unconnected). The drive DOES expose ALM and PEND;
  plan: land them on the broken-out pads GPIO34/35 (`board::DRIVE_ALM` /
  `DRIVE_PEND`). The full board+drive connector map is
  `docs/pedals-board-map.md`.
  **Still blocked before real hardware** (per the brief's own "UNVERIFIED"
  flags, not resolved by reading the manual alone):
  - **Physical layer**: the PCBA v2.2b puts a MAX3232 between the ESP32 UART
    and its RS232 port, so through the board's CN3 the levels are true
    ±RS232 by construction — the old "scope before wiring a 3.3V UART"
    caution only applies if you bypass the board's port. The drive side is
    labeled RS232 and presumed true-level; a scope check before first
    connect is still cheap insurance.
  - **Serial parameters**: baud, parity, and slave ID are not published (the
    manual defers to the vendor's "Protuner" tool) — must be discovered on
    the bench by probing candidate bauds (9600/19200/38400/57600/115200,
    8N1) and slave IDs (1-31), success = reading register 0 returns 57 (the
    drive-model identity value). Do not assume the iSV57T's 38400 8N1.
  - FC 0x10 (write-multiple-registers) support is unconfirmed for this
    drive; if unsupported, 32-bit register pairs (accel/decel/speed/target
    position) fall back to two sequential FC 0x06 writes.
  `pith-pedals-core/src/servo_jss57p.rs` implements the register map, the
  identity probe, and write-guards for the manual's do-not-write registers
  (motor type, factory reset) and read-only registers, with unit tests
  against `modbus.rs`'s framing — but it is **not wired into the firmware
  runtime loop**. The next real step is the user running the bench
  discovery/verification sequence (identity probe → bulk register compare
  → word-order check on target-position registers → single-register write
  test on the filter-time register → an unloaded motion test) before
  `runtime.rs` calls into any of this.
- **Pin map**: reproduced verbatim from the `PCB_VERSION == 9` block into
  `firmware/pedals/src/board.rs`, including two pin-reuse cases
  (GPIO4: MCP4725 DAC I2C SCL *and* brake-resistor control; GPIO6: ADC RST
  *and* emergency-stop) that exist in the reference source itself — see
  that module's doc comment before wiring anything to them.
- Board-specific quirks from that repo's README worth carrying over: the
  v2.x line requires a hand-soldered brake resistor (not populated by
  assembly), and first-flash entry uses a "Flash" button (not "Boot") — or
  hands-free via the onboard CH343P (see `docs/pedals-board-map.md`). The
  v2.1's pedal-type DIP switch (SW1) is GONE on the v2.2b — pedal role is
  software-assigned, which matches pith's identity-via-USB-serial approach.

## 1. What the reference project actually is

Bigger than a firmware+plugin pair — it's two ESP32 targets, a shared C
protocol library, and a WPF SimHub plugin:

- **`ESP32/`** — the pedal controller itself (ESP32-S2, since it does
  `USB_JOYSTICK` HID directly). Owns: an **ADS1220** loadcell ADC (TI 24-bit,
  via the `ADS1220_WE` library, on a dedicated SPI bus with a hardware DRDY
  falling-edge interrupt for sample-ready — NOT an HX711; confirmed by
  grepping the actual source, there's zero HX711 anywhere in this project),
  either a stepper (`StepperWithLimits.cpp`, on `ChrGri/FastNonAccelStepper`
  — the maintainer's own fork, not upstream FastAccelStepper) or an iSV57
  integrated servo (`isv57communication.cpp`, Modbus RTU over a UART at
  38400 baud) actuator, admittance (impedance) force control, ABS/RPM/
  bite-point/G/wheel-slip/road-impact/custom-vibration effect oscillators,
  USB HID joystick output, USB CDC + ESP-NOW wireless command/telemetry
  channel, EEPROM config persistence, OTA (both a pull-URL updater and
  PlatformIO-upload style).
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

**Phase 1 — protocol + effects + control math + dashboard UI (DONE).**
Mechanically portable, verifiable without hardware, no motor safety
implications — all of this is ported and host-unit-tested in
`pith-pedals-core` (~65 tests):
- Config/action/state data model (JSON-encoded, see §1's rationale), the
  11-point force curve + interpolation + gradient (`curve.rs`), and every
  effect oscillator the reference has: ABS, RPM, bite-point, G-force,
  wheel-slip, road-impact, and 4 custom-vibration slots (`effects.rs`),
  each a verbatim port of the matching `ESP32/include/*.h` class.
- **The full control stack**, ported function-by-function from the reference
  C++ and unit-tested on the host (this is what makes the pedal feel match
  or beat the reference): linkage forward/inverse kinematics + loadcell→
  pedal-face force conversion (`kinematics.rs` ← `PedalGeometry.h`), the
  constant-velocity / constant-acceleration Kalman filters + exponential
  filter (`filter.rs` ← `SignalFilter_1st/2nd_order.cpp`), the ADS1256
  loadcell scaling + Welford bias/variance estimator (`loadcell.rs` ←
  `LoadCell.cpp`), and the whole admittance model — Tustin integration, soft
  endstop, Coulomb friction, regen power clamp, velocity choke, soft leash,
  the Landi-et-al oscillation detector and position-gated virtual-mass
  adaptation (`admittance.rs` ← `StepperMovementStrategy.h`). A `controller.rs`
  ties raw ADC code → kg → lever → filter → effects → admittance → joystick
  in one host-tested per-tick pipeline (the reference's `pedalUpdateTask`).
- `firmware/pedals`: boots, exposes the report-id-2 command channel +
  `@CAP`/`@OTA`/`@CFG`/`@ACT`/`@ARM`/`@DISARM`/`@HOME`, and wires the real
  hardware — the **ADS1256 loadcell driver** (`ads1256.rs`, SPI + DRDY) and
  the **JSS57P1.5N Modbus servo driver** (`servo.rs` over `pith-pedals-core`'s
  tested framing) — into the control loop in `main.rs`. Compiles clean for
  `xtensa-esp32s3-espidf`. The loadcell→joystick path runs immediately (it
  commands nothing); motor output is disarmed by default (see Phase 2).
- Dashboard: `pith_device::Pedals` transport, the Pedals page with an
  **interactive force-curve editor** (draggable cubic-spline points +
  Linear/S-Curve/Exponent/Logarithm presets + max-force/preload/travel
  framing, mirroring the SimHub plugin), effect-gain sliders, named
  profiles, and **per-game/per-car auto profile switching**
  (`resolve_auto_profile`, the reference's `ApplyProfileAutoForCar/Game`).
- Telemetry: lateral/longitudinal G, per-wheel slip, and suspension impact
  are decoded per-game (field ids 90-93) from each source that carries them,
  with offsets cited/verified per game rather than guessed, and fed into the
  effects engine's `PedalAction`.

**Phase 2 — arm the motor on the bench (needs your hardware).** All the drive
code exists and compiles; what's left is operator-supervised validation
before it commands a real actuator, gated behind explicit steps so nothing
moves by accident:
1. **Scope the JSS57P1.5N's RS232 tuning port** — confirm TXD/RXD are 5 V TTL,
   not true ±RS232, before wiring to the ESP32 UART (§0).
2. **Discover serial params** — baud/parity/slave ID are unpublished; the
   servo driver's `probe_identity` reads register 0 (expect 57) to find them.
3. **Verify writes** on the safe filter-time register, then run an unloaded
   motion test, then `@HOME` with the measured endstop sweep values.
4. Only then `@ARM` — until armed, `servo.rs` refuses every motion command
   and `main.rs` never forwards a target. `@DISARM` and the servo's
   `hard_stop` (which bypasses the arm gate) are always available as e-stops.

**Phase 3 — wireless (ESP-NOW multi-pedal bridge)**, if the wired v1 proves
out and you still want it — see §4 for the *WiFi* (not ESP-NOW) transport,
which is a separate, simpler ask already in progress for all Pith devices.

## 4. WiFi transport + wireless axis (IMPLEMENTED for pedals)

The reference project's wireless story is ESP-NOW (connectionless 2.4 GHz,
no router/IP, pedal→bridge-dongle hop) with the bridge presenting the USB
HID joystick to the game. Pith takes the **virtual-joystick-over-WiFi**
route instead (the reference's optional vJoy path, done natively): the device
streams its axis over UDP and the dashboard feeds a **software virtual
joystick** the game reads — no bridge dongle, fully cable-free input.

**Wire protocol** (`pith_core::net`, shared by firmware + dashboard, UDP,
text): device broadcasts a `PITH <kind> <serial> <fw>` discovery beacon; the
dashboard replies `@SUB`; the device then streams `AX <serial> <value>`
(0..65535 axis, ~200 Hz) + `ST <serial> <status>` and accepts the same
`@CFG`/`@ACT`/`?`/`@ARM`/… commands it takes over USB (replies come back as
`RE <serial> <text>`). Ports: 42424 device→dashboard, 42425 dashboard→device.

**Implemented:**
- **Virtual joystick** (`dashboard/src/vjoy.rs`): Linux `/dev/uinput` via raw
  `libc` ioctls (no extra crate), up to 8 axes, 0..65535 range matching the
  USB HID axis. Verified registering with the kernel as a real input device.
  Non-Linux is a stub; a Windows ViGEm/vJoy backend slots behind the same
  `VirtualJoystick` type.
- **Dashboard transport** (`dashboard/src/wifi.rs`): discovers devices,
  subscribes, routes each device's axis into the virtual joystick, and
  forwards the live `$` telemetry frame to any wireless DDU.
- **Gated on WiFi input mode** (`State::wifi_input_enabled`, a Pedals-page
  toggle): OFF (default) → devices are still discovered but the axis is NOT
  routed and NO virtual joystick is created; the device's own USB HID axis is
  what the game reads (avoids double-reporting). ON → wireless axis feeds the
  virtual joystick, no USB cable needed for the game.
- **Firmware** (`firmware/pedals/src/wifi.rs`): `esp_wifi` STA with
  NVS-stored credentials, beacon + subscribe + axis/state stream + command
  relay, on its own thread. Compile-verified for `xtensa-esp32s3-espidf`;
  needs on-device/network bench validation like the servo path.
- **Provisioning**: `@WIFI <ssid> <password>` over USB (dashboard Pedals-page
  fields → `Pedals::provision_wifi`) → device persists to NVS and connects,
  no reboot.

**All devices + full routing (DONE, second pass):**
- The device-side transport is one shared crate — `firmware/pith-fw-wifi` —
  used by all three firmwares (pedals/handbrake stream their axis; the DDU
  instead *receives* `$` telemetry frames over UDP, making it a fully
  wireless dash). Production-hardened: the WiFi thread retries failed
  connects (10 s backoff), reconnects on AP drop, and applies live `@WIFI`
  re-provisioning without a reboot. Every firmware answers its full
  `@`-command protocol over UDP through the same dispatcher USB uses (the
  DDU has a `Transport::Wifi` variant; pedals/handbrake share
  `handle_command`).
- Dashboard routing for wireless-only devices: when no USB pedal is present
  but one is on the network, the effects stream (`@ACT`, 50 Hz) and config
  pushes (`@CFG`) go over UDP (`Ctx::send_wifi` → `wifi_loop` →
  device), and `RE` replies surface in the config-status line.
- **OTA over WiFi**: implemented as a TCP pull for reliability — the
  dashboard serves the image on an ephemeral TCP port and sends
  `@OTAWIFI <port> <size>` over UDP; the device connects back and streams
  the image into its normal OTA state machine (`pith-fw-wifi::OtaHooks` →
  each firmware's `ota::begin/feed`), then reboots via the usual
  `should_reboot` path. The handbrake's "install update" flow uses this
  automatically when the device is wireless-only.
- **Multi-pedal UI**: the Pedals screen has a Clutch/Brake/Throttle
  selector (the SimHub plugin's pattern) rebinding one shared editor to
  per-role config slots; a config pulled from a device lands in the slot
  matching its `pedal_type` and switches the selector to it.
- Settings persistence: the WiFi-input toggle and profile auto-switch
  persist across dashboard restarts (udp.json).

**Still bench-gated** (needs real hardware, like the servo path): the WiFi
firmware is compile-verified for both Xtensa targets but hasn't run on a
real network yet — validate STA connect, discovery, axis latency, and an
@OTAWIFI flash on the bench before calling the wireless path shipped.

## 5. PID allocation

Adds to `pith-device`'s table (`pith-device/src/lib.rs`):

```rust
pub const PID_PEDALS: u16 = 0x8002;
```
