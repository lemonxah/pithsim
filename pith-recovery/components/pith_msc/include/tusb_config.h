// TinyUSB device configuration for the pith-recovery USB drive (MSC only).
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

// One MSC interface, nothing else — recovery's USB drive is single-purpose.
#define CFG_TUD_CDC             0
#define CFG_TUD_HID             0
#define CFG_TUD_MSC             1
#define CFG_TUD_MIDI            0
#define CFG_TUD_VENDOR          0

// One full 512-byte sector per transfer.
#define CFG_TUD_MSC_EP_BUFSIZE  512
