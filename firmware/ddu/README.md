# pithddu-firmware

SimHub sim-racing dashboard firmware for an **ESP32-S3 (Seeed XIAO S3)** driving
**two 4" ST7796 SPI touch displays**, a **WS2812 rev/TC/ABS strip**, and an
on-screen **HID button box**. SimHub telemetry arrives over **USB CDC**; the
same USB cable also enumerates a **HID gamepad** (composite device) so the game
sees a controller for the touch buttons. It renders everything locally with
**LovyanGFX**.

Targets rF2 / Le Mans Ultimate, but any SimHub game works if you map the fields.

> **Companion desktop app:** [pithddu-dashboard](https://github.com/lemonxah/pithddu-dashboard)
> configures the device, builds/flashes this firmware, and can install a published
> release straight onto the device. The two repos are **versioned independently**.
>
> **Single-screen tier:** ESP32-**S2** (`devkit_s2`, `s2_mini`) is supported for a
> one-display build (4 MB flash, dual-OTA). Two displays need the S3's speed + PSRAM.

## Hardware

| XIAO S3 | Net | Notes |
|---|---|---|
| GPIO7 (D8)  | SPI SCLK | shared by both displays + both touch |
| GPIO9 (D10) | SPI MOSI | shared |
| GPIO8 (D9)  | SPI MISO | shared (touch only) |
| GPIO1 (D0)  | DISP1 CS | |
| GPIO2 (D1)  | DISP1 DC | |
| GPIO3 (D2)  | DISP2 CS | |
| GPIO4 (D3)  | DISP2 DC | |
| GPIO5 (D4)  | TOUCH1 CS | XPT2046 #1 |
| GPIO6 (D5)  | TOUCH2 CS | XPT2046 #2 |
| GPIO43 (D6) | WS2812 DIN | 14-LED chain (RMT) |
| 5V / GND    | — | feed displays + LED strip from 5V; **LEDs from an external 5V**, common ground |

Display RST → 3V3, BL → 3V3. Both panels share **SPI2** at 40 MHz; touch runs
at 1 MHz on the same bus. Pins are defined in `lgfx_setup.hpp` (displays),
`led_rev.c` (LED GPIO) and the USB is the S3's native OTG (no GPIO cost).

**LED chain order:** `[0..9]` rev counter, `[10..11]` TC (left), `[12..13]`
ABS (right). Brightness is capped in `led_rev.c` (`BRIGHT`).

## SimHub Custom Serial "update message"

One line, `$` then `;`-separated integers (times in ms; delta signed). The
first 4 fields are required; all later fields are optional and default to 0
(tyre wear defaults to 100). Fixed-point fields are scaled to integers and the
firmware scales them back. SimHub TimeSpans → ms via `timespantoseconds(...)*1000`
(SimHub's documented conversion; `TimeSpan * 86400000` yields another TimeSpan,
not a number, so `format()` of it is empty).

```
'$' + [Gear]
  + ';' + format(isnull([SpeedKmh],0),'0') + ';' + format(isnull([Rpms],0),'0') + ';' + format(isnull([CarSettings_MaxRPM],0),'0')
  + ';' + format(isnull([CarSettings_RPMShiftLight1],0),'0')              // shift point
  + ';' + format(timespantoseconds(isnull([CurrentLapTime],secondstotimespan(0)))*1000,'0') + ';' + format(timespantoseconds(isnull([LastLapTime],secondstotimespan(0)))*1000,'0')
  + ';' + format(timespantoseconds(isnull([BestLapTime],secondstotimespan(0)))*1000,'0')                    // session best
  + ';' + format(timespantoseconds(isnull([PersonalBestLapTime],secondstotimespan(0)))*1000,'0')
  + ';' + format(timespantoseconds(isnull([EstimatedLapTime],secondstotimespan(0)))*1000,'0')
  + ';' + format(isnull([PersistantTrackerPlugin.SessionBestLiveDeltaSeconds],0)*10000,'0')  // signed, 0.1ms units (4 decimals, clamped +/-9.9999 on device)
  + ';' + [Position] + ';' + ([OpponentsCount]+1)
  + ';' + [CurrentLap] + ';' + [TotalLaps] + ';' + [RemainingLaps]
  + ';' + format(isnull([WaterTemperature],0),'0') + ';' + format(isnull([OilTemperature],0),'0')
  + ';' + format(isnull([OilPressure],0)*10,'0')
  + ';' + format(isnull([TurboPercent],0),'0') + ';' + [TCLevel] + ';' + [ABSLevel]
  + ';' + format(isnull([BrakeBias],0)*10,'0')
  + ';' + format(isnull([Fuel],0)*10,'0') + ';' + format(isnull([CarSettings_FuelCapacity],0)*10,'0')
  + ';' + format([Computed.Fuel_LitersPerLap]*1000,'0')
  + ';' + format([Computed.Fuel_RemainingLaps]*10,'0')
  // tyre temps inner;mid;outer per corner (FL,FR,RL,RR)
  + ';' + ttFLi + ';' + ttFLm + ';' + ttFLo + ';' + ttFRi + ';' + ttFRm + ';' + ttFRo
  + ';' + ttRLi + ';' + ttRLm + ';' + ttRLo + ';' + ttRRi + ';' + ttRRm + ';' + ttRRo
  + ';' + tpFL + ';' + tpFR + ';' + tpRL + ';' + tpRR        // pressures (kPa)
  + ';' + twFL + ';' + twFR + ';' + twRL + ';' + twRR        // wear (%)
  + ';' + btFL + ';' + btFR + ';' + btRL + ';' + btRR        // brake temps (C)
  + ';' + format(isnull([Throttle],0),'0') + ';' + format(isnull([Brake],0),'0') + ';' + format(isnull([Clutch],0),'0') + ';' + steer
  + ';' + tcActive + ';' + absActive                          // 0/1 live engagement
  + ';' + format(isnull([Headlights],0),'0') + ';' + format(isnull([Wipers],0),'0') + ';' + format(isnull([PitLimiterOn],0),'0') + ';' + format(isnull([IgnitionOn],0),'0')  // car control states (button-box sync)
  + ';' + format(posX*100,'0') + ';' + format(posZ*100,'0')             // world pos for the map
  // sector times (ms): this/last lap, then personal-best sectors (for color)
  + ';' + format(timespantoseconds(isnull([Sector1LastLapTime],secondstotimespan(0)))*1000,'0') + ';' + format(timespantoseconds(isnull([Sector2LastLapTime],secondstotimespan(0)))*1000,'0') + ';' + format(timespantoseconds(isnull([Sector3LastLapTime],secondstotimespan(0)))*1000,'0')
  + ';' + format(timespantoseconds(isnull([Sector1BestLapTime],secondstotimespan(0)))*1000,'0') + ';' + format(timespantoseconds(isnull([Sector2BestLapTime],secondstotimespan(0)))*1000,'0') + ';' + format(timespantoseconds(isnull([Sector3BestLapTime],secondstotimespan(0)))*1000,'0')
```

Gear may be a letter (`N`/`R`/`1`..`9`) **or** numeric (`0`=neutral, `-1`=reverse) —
the firmware accepts both.

Property names vary by game/plugin — verify in SimHub's picker. 3-zone tyre
temps, the fuel calculator, steering and TC/ABS-active in particular differ per
game (LMU 1.3+ exposes extended TC/ABS). For TC/ABS-active you can use the
native flag or the throttle delta (`Unfiltered − Filtered`) / `WheelLock`.

## Displays

- **Display 1 (driving):** RACE HUD (gear, speed, position, fuel, lap times,
  delta bar). Tap toggles to the **self-learned track MAP** (records your line
  on lap 1, then draws outline + a live car dot).
- **Display 2 (data + controls):** tab bar **TYRE / BRK / PIT / FUNC**.
  TYRE = 3-zone temps + pressure + wear per corner; BRK = brake temps + water/
  oil; PIT / FUNC = config-driven button pages (below).

All threshold colors (tyre/brake temps, wear, water/oil, delta, fuel) are
computed in firmware — SimHub only sends numbers.

## Button box + profiles

A composite USB **HID gamepad** (32 buttons) is enumerated alongside CDC. Touch
a button on a PIT/FUNC page → the firmware fires that HID button → bind it
in-game (pit menu = "LCD up/down/inc/dec" + Request Pit; functions = lights,
wipers, limiter, ignition, hybrid, ...).

Button pages are **not hardcoded** — they come from a **profile** (per game).
The firmware ships a default LMU/rF2 profile and persists pushed profiles to NVS.
Design and push button pages from the [dashboard app](https://github.com/lemonxah/pithddu-dashboard)'s
**Buttons** screen; the device applies them live and they survive reboots.

Wire format: the device accepts a one-line command `@P{json}` on the CDC port.
`{json}` is `{"game":"...","pages":[{"name":"PIT","buttons":[{"x","y","w","h",
"color"(RGB565),"action"(1=HID,2=hold,3=page,4=peek),"param"(HID btn),"label"}]}]}`.
Send `?` for a status line.

## Build & flash

This firmware is **Rust** (Cargo + esp-idf-sys, std), targeting the **ESP32-S3
(XIAO S3)**. Install the esp Rust toolchain once with [`espup`](https://github.com/esp-rs/espup)
(`espup install`), which writes `~/export-esp.sh`.

```
source ~/export-esp.sh
just build                      # cargo build
just flash                      # build release + espflash flash --monitor
just test                       # host unit tests (pith-core)
just image                      # save the bare app .bin (what the dashboard installs)
```

(Or use `cargo` directly: `cargo build` / `espflash flash --monitor target/xtensa-esp32s3-espidf/release/pithddu`.)

The first build checks out and compiles ESP-IDF v5.3.3 under `.embuild/` (a few
minutes); subsequent builds are incremental. Only the **XIAO S3** board is
currently supported by the Rust port (the legacy multi-board C build is in git
history before the `rust-rewrite` branch).

The [dashboard app](https://github.com/lemonxah/pithddu-dashboard)'s **Firmware** tab
can **FLASH THIS BUILD** over USB or download + flash a **published version**
(CI publishes a bare `pithddu-xiao_s3.bin` per release).

The USB port is the SimHub CDC link; **console logs go to UART0** (attach a
3.3V USB-UART adapter to see them). Because the running firmware owns the
USB-OTG as a CDC device, esptool's auto-reset can't enter download mode — for a
**reflash, put the XIAO in download mode manually**: hold **BOOT (B)**, tap
**RESET (R)**, release **BOOT**. It re-enumerates as `303a:1001` and flashes
normally.

## Releases & CI

GitHub Actions (`.github/workflows/build.yml`) builds every board on each push and,
on a **`v*` tag**, publishes a **GitHub Release** with per-board assets:
`pithddu-<board>.bin` (bare app image for OTA) and `pithddu-<board>.zip`
(bootloader + partition table + app + `flash.txt`). The dashboard's Firmware tab
reads these releases so you can pick a version to flash.

Cut a release with the [`just`](https://github.com/casey/just) recipe:

```
just release          # bump the patch of the latest vX.Y.Z tag
just release 1.2.3    # tag exactly v1.2.3
```

It tags + pushes; CI does the rest. `FW_VERSION` (in `main/ui.cpp` and the `@CAP`
reply in `main/simhub_main.c`) is what the device reports — bump it to match the tag.

## Desktop app + firmware updates (transport)

The [dashboard app](https://github.com/lemonxah/pithddu-dashboard) talks to the
device **only over the HID command channel (report id 2)** — never CDC — so the CDC
port stays free for SimHub and
**firmware updates always have a working transport** (OTA streams over HID in
~61-byte report chunks; slower than CDC but SimHub-safe). The connection card
reads *"Connected · HID (SimHub-safe)"*.

On **Linux** the app needs `hidraw` permission. Install the bundled udev rule
(VID `303a` / PID `4002`):

```
sudo cp tools/99-pithddu.rules /etc/udev/rules.d/99-pithddu.rules
sudo udevadm control --reload-rules && sudo udevadm trigger   # then re-plug
```

Windows needs nothing (HID is plug-and-play).
