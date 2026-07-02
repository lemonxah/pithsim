// Thin C shim over raw TinyUSB for the pith-hb HID-only composite device.
//
// Why a C shim: esp_tinyusb cannot be bindgen'd here (FreeRTOS-OSAL + packed/
// align walls), so all TinyUSB contact lives in this component behind a
// clean, tinyusb-type-free C API. Rust (src/usb.rs) drives it.
//
// No CDC, no COM port — this is a plain HID device with two reports:
//   - Report ID 1: one 16-bit axis (Slider usage) + 1 placeholder button
//                   (always released, no physical button wired — added so
//                   Steam's controller detection doesn't ignore an axis-only
//                   joystick), IN-only. This is what games/Windows/SimHub see
//                   as a generic joystick axis.
//   - Report ID 2: vendor IN/OUT command channel ([len][payload], chunked),
//                   used ONLY by the pith-hb-dashboard app for the
//                   calibration protocol + raw/telemetry streaming. Never
//                   exposed to games.
#pragma once

#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

// Bring up the USB OTG PHY, init TinyUSB, and start the device task. `serial`
// is the device serial string (e.g. "PITHHB-XXXXXXXXXXXX") used in the USB
// serial string descriptor; it is copied internally.
void pith_hb_usb_init(const char *serial);

// True once the host has enumerated/configured the device.
bool pith_hb_usb_mounted(void);

// True when the HID IN endpoint can accept a report right now.
bool pith_hid_ready(void);

// Push the current axis value (0..=65535) as report id 1. Returns true if queued.
bool pith_hid_send_axis(uint16_t value);

// Send an HID IN report (report id 2 = command-reply / telemetry channel).
// Returns true if the report was queued.
bool pith_hid_send(uint8_t report_id, const void *data, int len);

// NOTE: the device -> Rust callbacks (pith_on_hid_cmd / pith_on_hid_tx_complete)
// are implemented in Rust (#[no_mangle]) and declared inside pith_hb_usb.c —
// intentionally NOT declared here so bindgen doesn't emit extern decls that
// collide with the Rust definitions.

#ifdef __cplusplus
}
#endif
