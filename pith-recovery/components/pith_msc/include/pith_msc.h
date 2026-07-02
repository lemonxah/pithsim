// pith_msc — recovery-app USB mass storage (read-only RAM disk).
//
// The Rust side builds a small FAT12 image in RAM (files = the device's saved
// NVS config blobs) and hands it here; this shim brings up the USB OTG PHY +
// TinyUSB with a single write-protected MSC LUN served straight from that
// buffer. There is no teardown: the caller exits USB-drive mode by rebooting
// (the recovery menu's flow), which is the simplest safe way to detach.
#pragma once

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Start USB MSC serving `disk` (`size` bytes, multiple of 512). The buffer must
// stay alive forever (leak it). Returns false if USB bring-up failed.
bool pith_msc_start(const uint8_t *disk, uint32_t size);

#ifdef __cplusplus
}
#endif
