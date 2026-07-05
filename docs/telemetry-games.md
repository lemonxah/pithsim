# Game telemetry notes — coverage, CarX, EA SPORTS WRC

Where each game's FFB-relevant channels (longitudinal/lateral G, per-wheel
slip, suspension velocity) come from, plus the findings on the two titles
that needed research. Field ids 90-93 (`g_long_x100`, `g_lat_x100`,
`wheel_slip`, `susp_impact`) carry these through the normal telemetry merge
to the active-pedal effects engine (`dashboard/src/pedals.rs::build_action`).

## FFB-channel coverage by source

| Source | G long/lat | wheel slip | susp impact | notes |
|---|---|---|---|---|
| AC / ACC shared memory | ✅ accG@44 | ✅ wheelSlip@56 | — (position only) | via pith-shm-bridge |
| rF2 / LMU shared memory | ✅ mLocalAccel@208 (sign-fixed) | ✅ 1−mGripFract proxy | — (deflection only) | |
| RaceRoom shared memory | ✅ @1440 | ✅ tire_speed vs body | ✅ susp velocity @424 | |
| PCARS2 / AMS2 UDP | ✅ @100/108 | — (no tyre radius on wire) | ✅ sSuspensionVelocity@328 | |
| Forza UDP | ✅ @20/28 | ✅ TireSlipRatio@84 | — (position only) | |
| DiRT / GRID UDP (extradata=3) | ✅ idx 34/35 | ✅ wheel speeds vs body | ✅ susp velocity idx 21-24 | |
| **EA SPORTS WRC UDP** | ✅ accel@73/81 | ✅ cp_forward_speed@153 | ✅ hub_velocity@137 | **new decoder**, see below |
| F1 2x UDP | ✅ Motion pkt 0 | ✅ MotionEx pkt 13 | ✅ MotionEx susp velocity | |
| GT7 | derived (Δspeed) | ✅ wheel rps × radius | — | |
| AC UDP (RTCarInfo) | ✅ accG@28-36 | ✅ slipRatio@132 | — | |
| OutGauge (LFS/BeamNG) | derived (Δspeed) | — | — | OutSim not implemented |
| ACC broadcasting | derived (Δspeed) | — | — | protocol carries no physics |

"derived" = `dashboard/src/telemetry/derive.rs` computes g_long from frame-to-
frame Δspeed when the source leaves it 0.

## EA SPORTS WRC (2023) — SUPPORTED via the new `eawrc.rs` decoder

EA WRC does **not** reuse the old DiRT "extradata" UDP array (a prior module
doc claimed it did — fixed). It has its own JSON-configurable UDP system; the
`pith-sim/src/eawrc.rs` decoder parses the game's **default** packet
(structure `wrc`, packet `session_update`, 237 bytes) which carries
everything the pedals need: acceleration in m/s², per-wheel contact-patch
forward speed (slip), per-wheel suspension hub velocity (impact), true RPM,
gear, pedals, stage time/distance.

**User setup** (one-time):
1. Edit `Documents/My Games/WRC/telemetry/config.json`.
2. In the `"wrc"` / `"session_update"` entry set `"bEnabled": true`.
   Default target is `127.0.0.1:20777`; the dashboard's UDP listener port is
   what you point it at (or set `"ip"` to the Linux host's address for a
   two-PC setup).

**Linux caveat**: since game patch v1.9 (July 2024) EA's kernel anticheat
prevents EA WRC itself from running under Proton/Wine at all. In practice
this decoder serves a **two-PC setup** (Windows game box → UDP → Linux
dashboard host) or pre-1.9 offline installs.

## CarX Drift Racing Online — no telemetry exists (yet); the viable path

Researched thoroughly (Dec 2025-current):
- **No native output.** No UDP, no shared memory, no files. The only motion
  output is a closed D-Box integration (confirmed by a CarX dev on Steam,
  Dec 2025).
- **No tool supports it** — not SimHub, DashPanel (explicitly lists it as
  "no telemetry interface"), Sim Racing Studio, SimTools, or SpaceMonkey.
  No community exporter mod exists on GitHub either.

**The practical path** (not yet built): CarX DRO is a Unity/Mono game with a
mature BepInEx 5 modding scene (kino, VORTEX prove the vehicle physics
objects are reachable from C# plugins, including suspension data). A small
BepInEx plugin (~150 lines) can sample the player car each `FixedUpdate` and
fire UDP datagrams at the dashboard. Two wire options:
1. **Emit the DiRT extradata=3 264-byte float array** → pith's existing
   `codemasters.rs` decoder picks it up **unchanged** (SpaceMonkey validates
   this exact pattern — it uses the DiRT-4 format as a lingua franca).
2. Emit pith's own `$` text frames.

Under Proton, BepInEx installs with `WINEDLLOVERRIDES="winhttp=n,b" %command%`
and UDP from inside the prefix reaches the Linux host directly — **no
pith-shm-bridge needed**. The mod is the only new work; pithsim needs zero
changes if option 1 is chosen.
