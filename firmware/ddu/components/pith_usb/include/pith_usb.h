// Thin C shim over raw TinyUSB for the pithddu composite USB device.
//
// Why a C shim: esp_tinyusb cannot be bindgen'd here (FreeRTOS-OSAL + packed/
// align walls), so all TinyUSB contact lives in this component behind a clean,
// tinyusb-type-free C API. Rust (src/usb.rs) drives it and implements the two
// callbacks below. The protocol/OTA/NVS logic all stays in Rust.
//
// Composite layout (matches the legacy firmware so the dashboard is unchanged):
//   - CDC-ACM  : SimHub Custom Serial telemetry ('$' frames) + '@CM' car model
//   - HID id 1 : 32-button joystick (the on-screen button box)
//   - HID id 2 : vendor IN/OUT command channel for the PC app ([len][payload])
#pragma once

#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

// Bring up the USB OTG PHY, init TinyUSB, and start the device task. `serial`
// is the device serial string (e.g. "PITH-XXXXXXXXXXXX") used in the USB serial
// string descriptor; it is copied internally.
void pith_usb_init(const char *serial);

// True once the host has enumerated/configured the device.
bool pith_usb_mounted(void);

// ---- CDC (SimHub telemetry channel, interface 0) ----
// Read up to `max` bytes from the CDC RX FIFO; returns the count read (0 if none).
int  pith_cdc_read(uint8_t *buf, int max);
// Queue `len` bytes to the CDC TX FIFO; returns bytes accepted.
int  pith_cdc_write(const uint8_t *buf, int len);
// Flush queued CDC TX bytes to the host.
void pith_cdc_flush(void);

// ---- HID ----
// True when the HID IN endpoint can accept a report.
bool pith_hid_ready(void);
// Send an HID IN report (report_id 1 = joystick, 2 = command-reply channel).
// Returns true if the report was queued.
bool pith_hid_send(uint8_t report_id, const void *data, int len);

// NOTE: the device -> Rust callbacks (pith_on_hid_cmd / pith_on_hid_tx_complete)
// are implemented in Rust (#[no_mangle]) and declared inside pith_usb.c — they
// are intentionally NOT declared here so bindgen doesn't emit extern decls that
// collide with the Rust definitions.

#ifdef __cplusplus
}
#endif
