# PCBA v2.2 Rev B — complete board map

The pedal's control board: gilphilbert's integrated control+power PCBA
(`gilphilbert/DIY-Sim-Racing-FFB-Pedal-PCBs`, `v2/v2.2/RevB`), 91×51 mm.
Everything below is sourced, not guessed — from that revision's BOM
(`BOM_ffb-pedal-integrated_v2.2b.csv`), the v2 family readme, the reference
firmware's `Main.h` `PCB_VERSION == 9` pin block (reproduced in
`firmware/pedals/src/board.rs`), the project wiring diagrams, and what the
physical board enumerated on the bench. Where something could not be pinned
to a source it is marked **confirm on silkscreen**.

## Modules on the board

| RefDes | Part | What it is | Connected to |
|---|---|---|---|
| U5 | ESP32-S3FH4R2 | MCU — 4 MB flash, 2 MB PSRAM, WiFi/BLE | everything below |
| U9 | SL2.1S | **USB 2.0 hub** behind the USB-C port | USB-C ↔ U7 + U5 native USB |
| U7 | CH343P | **onboard USB-UART bridge** — UART0 console + hands-free flashing (Q2/Q3 auto-reset into download mode, no BOOT button) | hub ↔ GPIO43/44 (UART0) |
| U2 | ADS1255IDB | 24-bit load-cell ADC (driver-compatible with the ADS1256 — same SPI command set; AIN0/AIN1 differential is all we use) | SPI on GPIO16/17/18, CS 7, DRDY 15, RST 6 |
| X2 | 7.68 MHz crystal | the ADC's clock (matches the driver's `ADC_CLOCK_MHZ`) | U2 |
| U4 | REF5025 | precision 2.5 V reference for the ADC | U2 VREF |
| U8 | MAX3232 | RS232 transceiver — **true ±RS232 levels** on the servo serial port; the ESP's 3.3 V UART never touches the wire directly | GPIO10 (TX) / GPIO9 (RX) ↔ servo RS232 port |
| U10, U11 | LTV-217 ×2 | optocouplers isolating the step/dir outputs | GPIO38 (PUL) / GPIO37 (DIR) → servo signal port |
| Q5 | STD35P6LLF6 | P-FET high-side switch: servo power on/off ([FUTURE] in the reference firmware) | GPIO3 → XT30 out |
| Q1 + R25 | DOZ35N06 + 5 Ω 5 W | brake-resistor dump circuit (R25 is the hand-soldered wirewound resistor) | GPIO4 |
| LED | WS2812C-2427 | status LED — ONE addressable GRB pixel, NOT a plain LED | GPIO12 (`led.rs`) |
| BUZZER1 | MLT-7525 | buzzer, 2.7 kHz | GPIO21 |
| U12, U13 | tactile switches | FLASH (BOOT) + RESET buttons — the ONLY switches on this revision; the v2.1's pedal-type DIP (SW1) was dropped, pedal type is software-assigned (`PEDAL_SOFTWARE_ASSIGNMENT` in the reference firmware) | GPIO0 / EN |
| U1 | LMR16030 | 60 V buck: 24–48 V in → 5 V rail | PW1 → 5 V |
| U3 | AMS1117-3.3 | 5 V → 3.3 V LDO | 3.3 V rail |
| U14 | NCP360 | USB VBUS overvoltage protection switch | USB-C 5 V |
| U6 | TPD3E001 | USB ESD protection | USB data lines |
| RF1 | IPEX/u.FL | **external WiFi antenna connector — WiFi does not work without an antenna fitted** (no PCB antenna on this board) | U5 RF |
| C25 | 470 µF | optional bulk cap for extreme braking loads (usually not fitted) | brake circuit |
| X1 | 40 MHz osc | ESP32 clock | U5 |

## ESP32-S3 GPIO map

Everything from the reference `Main.h` `PCB_VERSION == 9` block, plus the
chip's fixed-function pins. GPIOs not listed are unconnected.

| GPIO | Signal | Goes to |
|---|---|---|
| 0 | FLASH/BOOT button (strapping) | U12 |
| 1 | CFG1 in the reference pin table — no DIP on v2.2b, effectively free | — |
| 2 | CFG2 in the reference pin table — no DIP on v2.2b, effectively free | — |
| 3 | SERVO_POWER_ENABLE (strapping — high-side FET) | Q5 → XT30 out |
| 4 | BRAKE_RESISTOR gate (reference pin table also lists it as MCP4725 SCL — the DAC is **not fitted** on this board, so the pin is the brake FET's) | Q1 |
| 5 | (MCP4725 SDA — DAC not fitted, effectively free) | — |
| 6 | ADC RST **and** EMRGNCY header (shared in the reference pin table) | U2 / emergency header |
| 7 | ADC chip select | U2 CS |
| 9 | UART1 RX (servo serial, behind MAX3232) | U8 |
| 10 | UART1 TX (servo serial, behind MAX3232) | U8 |
| 12 | WS2812 status LED data | LED |
| 15 | ADC DRDY (sample-ready, falling edge) | U2 |
| 16 | ADC SPI SCK | U2 |
| 17 | ADC SPI MOSI (DIN) | U2 |
| 18 | ADC SPI MISO (DOUT) | U2 |
| 19, 20 | USB D− / D+ (native USB-OTG) | U9 hub |
| 21 | buzzer | BUZZER1 |
| 33 | broken out (ESP-NOW pairing button in the reference firmware) | pad |
| 34 | broken out — PLANNED: the drive's ALM (alarm) output lands here (`board::DRIVE_ALM`); the board has no dedicated ALM terminal | pad |
| 35 | broken out — PLANNED: the drive's PEND (in-position) output lands here (`board::DRIVE_PEND`); the board has no dedicated PEND terminal | pad |
| 37 | DIR (via optocoupler) | U11 → servo signal port |
| 38 | PUL/STEP (via optocoupler) | U10 → servo signal port |
| 43, 44 | UART0 TX / RX — the console | U7 CH343P |

## USB topology — why the board shows up as TWO serial ports

```
USB-C ── U14 (VBUS OVP) ── U9 SL2.1S hub ─┬─ U7 CH343P (1a86:55d3)  → ttyACM*
                                          │    UART0 console + auto-flash
                                          └─ ESP32-S3 native USB    → ttyACM*
                                               ROM: 303a:1001 "USB JTAG/serial"
                                               app: 303a:8002 Pith Pedals HID
```

One cable, three `dmesg` entries: the hub (`1a40:0101`), the CH343
("USB Single Serial"), and the ESP. Practical consequences:

- **Firmware logs need no extra wiring** — the sdkconfig console is UART0,
  which is the CH343 port. `just monitor <CH343 port>` shows boot banners,
  reset reasons and panics.
- **Hands-free flashing** — the CH343's DTR/RTS drive the auto-download
  transistors, so `just pedals-flash <CH343 port>` works without holding
  BOOT. (Flashing via the native-USB port in ROM download mode also works,
  that one DOES need the BOOT button.)
- When the app is running, the native-USB port stops being 303a:1001 and
  becomes the Pith Pedals HID device (303a:8002).

## Connectors (board edge)

All signal headers are 2.54 mm screw terminals; servo power is XT30.

| Connector | Pins | Function |
|---|---|---|
| USB-C | — | power + data (the hub above) |
| PW1 (XT30 male) | 2 | 24–48 V supply in |
| PW2 (XT30 female) | 2 | switched servo power out (through Q5) |
| CN4 | 4 | servo signal: PUL+ PUL− DIR+ DIR− (optocoupled) — **confirm order on silkscreen** |
| CN3 | 3 | servo RS232: TX RX GND (true RS232 via MAX3232) — **confirm order on silkscreen** |
| CN1 | 5 | load cell: signal± / excitation± / shield (v1 marking: `S− S+ ⏚ ⏚ +`) — **confirm order on silkscreen** |
| CN5 | 2 | EMRGNCY — emergency cut-off to GPIO6 — **confirm on silkscreen** |
| CN2 (3.81 mm terminal) | 2 | high-current 2-pin terminal — **confirm role on silkscreen** |

## Servo interface — what the BOARD offers vs what the DRIVE has

The PCBA's entire servo-facing interface is:

```
PUL · DIR (CN4, optocoupled) · RX · TX · GND (CN3, RS232) · VCC · GND (PW2, switched 24–48 V)
```

**The BOARD has no ALM (alarm) or PEND (in-position) input terminals** — the
reference design simply leaves those drive outputs unconnected (its wiring
diagram runs only PUL±/DIR± and RS232 to the board). The drive on this build
— a **JSS57P1.5N** — exposes **RS232 plus PEND+/PEND− and ALM+/ALM−**
(confirmed on the unit). Consequences:

- To use the drive's ALM/PEND outputs, land them on the broken-out pads:
  **ALM → GPIO34, PEND → GPIO35** (`board::DRIVE_ALM` / `DRIVE_PEND`).
  Check the drive's output circuit first — open-collector pairs wire − to
  GND and + to the pad with the internal pull-up (active-low); a 5 V
  push-pull output needs a divider, the S3 pads are **not** 5 V-tolerant.
- Without that wiring, alarm / in-position state is only reachable **over
  RS232 Modbus** (`REG_MOTION_STATE` 32 for move-complete). The ALM/PEND
  configuration registers (15, 18, 19) set the polarity/function of the
  outputs you'd wire to those pads.
- The drive's PUL/DIR inputs vs the board's differential opto-isolated
  PUL±/DIR± outputs: how to bridge (common anode vs common cathode) depends
  on the drive's input circuit — **bench task with the drive manual, do not
  guess polarity**.
- RS232 is 3-wire: board TX → drive RX, board RX ← drive TX, GND–GND. The
  board side is true ±RS232 through the MAX3232, so the old "3.3 V UART vs
  RS232 levels" damage concern applies only if you bypass the board's port —
  through CN3 the levels are correct by construction. The remaining unknown
  is the drive's serial parameters (baud/parity/slave ID — bench discovery,
  `docs/pedals.md` §0, `pith-device`'s `jss_probe` example).
- VCC/GND on the drive is its 24–48 V motor supply — feed it from PW2 (the
  switched XT30) so GPIO3 servo-power control works, or straight from the
  PSU if not using that feature.

## Firmware notes

- The ADC is an **ADS1255**, not ADS1256 (BOM line 65). Same SPI commands,
  same driver (`ads1256.rs`); the 1255 just has fewer mux inputs — we only
  use AIN0/AIN1 differential anyway.
- WiFi (`pith-fw-wifi`) requires an antenna on RF1 — the S3FH4R2 has no
  onboard antenna. Without one, expect connects to fail/be flaky even with
  correct credentials.
- Pedal type is software-assigned on this revision (no DIP switch — the
  v2.1's SW1 is gone from the v2.2b BOM). The pith firmware already does it
  in software: pedal identity rides the USB serial (see `pith-device`),
  which matches this board's design.
- GPIO6 doubles as ADC reset and the emergency header — treat the EMRGNCY
  input as incompatible with pulsing ADC RST (the reference firmware lives
  with the same overlap).
