// TinyUSB device configuration for the pithddu composite device (CDC + HID).
// Consumed by the raw `espressif/tinyusb` component. OPT_* constants are defined
// by tusb_option.h before it includes this file.
#pragma once

#define CFG_TUSB_MCU            OPT_MCU_ESP32S3
#define CFG_TUSB_OS             OPT_OS_FREERTOS
#define CFG_TUSB_RHPORT0_MODE   (OPT_MODE_DEVICE | OPT_MODE_FULL_SPEED)

#ifndef CFG_TUSB_MEM_SECTION
#define CFG_TUSB_MEM_SECTION
#endif
#ifndef CFG_TUSB_MEM_ALIGN
#define CFG_TUSB_MEM_ALIGN      __attribute__((aligned(4)))
#endif

#define CFG_TUD_ENABLED         1
#define CFG_TUD_ENDPOINT0_SIZE  64

// Interface classes: one CDC (SimHub telemetry) + one HID (button box + the
// vendor command channel on report id 2).
#define CFG_TUD_CDC             1
#define CFG_TUD_HID             1
#define CFG_TUD_MSC             0
#define CFG_TUD_MIDI            0
#define CFG_TUD_VENDOR          0

#define CFG_TUD_CDC_RX_BUFSIZE  1024
#define CFG_TUD_CDC_TX_BUFSIZE  1024
#define CFG_TUD_CDC_EP_BUFSIZE  64

// HID endpoint buffer must hold a full 64-byte report (report id + 63 payload).
#define CFG_TUD_HID_EP_BUFSIZE  64
