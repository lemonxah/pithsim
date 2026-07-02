// pith_hb_usb — thin C shim over raw TinyUSB. See pith_hb_usb.h for rationale.

#include "pith_hb_usb.h"

#include <string.h>
#include "tusb.h"
#include "class/hid/hid_device.h"
#include "esp_private/usb_phy.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_log.h"

static const char *TAG = "pith_hb_usb";

// Implemented in Rust (src/usb.rs), declared here (not in the bindgen header).
extern void pith_on_hid_cmd(const uint8_t *buf, int len);
extern void pith_on_hid_tx_complete(void);

// ---- HID report descriptor ----
// Report ID 1: one 16-bit Slider axis + 1 placeholder button, IN-only — the
// game-facing joystick. The button is always reported released (no physical
// button wired — this is a single load-cell axis device); it exists because
// Steam's controller detection silently ignores axis-only HID joysticks with
// zero buttons, so a dummy button gets it past that filter.
// Report ID 2: vendor IN/OUT (63-byte) command channel — the calibration
// protocol + telemetry stream, used only by the pith-hb-dashboard app.
static const uint8_t s_hid_report[] = {
    // --- Joystick axis, Report ID 1 ---
    0x05, 0x01,        // Usage Page (Generic Desktop)
    0x09, 0x04,        // Usage (Joystick)
    0xA1, 0x01,        // Collection (Application)
    0x85, 0x01,        //   Report ID (1)
    0x09, 0x36,        //   Usage (Slider)
    0x15, 0x00,        //   Logical Minimum (0)
    0x27, 0xFF, 0xFF, 0x00, 0x00,  //   Logical Maximum (65535) — must be a
                       //   4-byte item: a 2-byte 0xFFFF sign-extends to -1,
                       //   making Logical Max < Logical Min and breaking HID
                       //   parsers' joystick/axis enumeration (this is why
                       //   raw hidapi I/O worked but games/Steam saw nothing).
    0x75, 0x10,        //   Report Size (16)
    0x95, 0x01,        //   Report Count (1)
    0x81, 0x02,        //   Input (Data,Var,Abs)
    0x05, 0x09,        //   Usage Page (Button)
    0x19, 0x01,        //   Usage Minimum (Button 1)
    0x29, 0x01,        //   Usage Maximum (Button 1)
    0x15, 0x00,        //   Logical Minimum (0)
    0x25, 0x01,        //   Logical Maximum (1)
    0x75, 0x01,        //   Report Size (1)
    0x95, 0x01,        //   Report Count (1)
    0x81, 0x02,        //   Input (Data,Var,Abs)  -- placeholder button
    0x75, 0x07,        //   Report Size (7)
    0x95, 0x01,        //   Report Count (1)
    0x81, 0x03,        //   Input (Const,Var,Abs) -- pad button byte
    0xC0,              // End Collection
    // --- Vendor command channel, Report ID 2 ---
    0x06, 0x00, 0xFF,  // Usage Page (Vendor-defined 0xFF00)
    0x09, 0x01,        // Usage (0x01)
    0xA1, 0x01,        // Collection (Application)
    0x85, 0x02,        //   Report ID (2)
    0x15, 0x00,        //   Logical Minimum (0)
    0x26, 0xFF, 0x00,  //   Logical Maximum (255)
    0x75, 0x08,        //   Report Size (8)
    0x95, 0x3F,        //   Report Count (63)
    0x09, 0x01, 0x81, 0x02,  //   Input  (Data,Var,Abs)  device -> host
    0x09, 0x01, 0x91, 0x02,  //   Output (Data,Var,Abs)  host -> device
    0xC0               // End Collection
};

// ---- Interface / endpoint / string-index layout ----
// A single HID interface — no CDC, so no IAD needed (that was only required
// to group CDC's two interfaces into one function for Windows).
enum {
    ITF_NUM_HID = 0,
    ITF_NUM_TOTAL
};
#define EPNUM_HID_OUT   0x02    // host -> device command reports
#define EPNUM_HID_IN    0x81    // device -> host (axis + command replies)
#define STRID_HID  4
#define CONFIG_TOTAL_LEN (TUD_CONFIG_DESC_LEN + TUD_HID_INOUT_DESC_LEN)

// ---- Device descriptor (plain single-function HID device) ----
static const tusb_desc_device_t s_device_desc = {
    .bLength            = sizeof(tusb_desc_device_t),
    .bDescriptorType    = TUSB_DESC_DEVICE,
    .bcdUSB             = 0x0200,
    .bDeviceClass       = 0x00,   // class is per-interface (standard HID device)
    .bDeviceSubClass    = 0x00,
    .bDeviceProtocol    = 0x00,
    .bMaxPacketSize0    = CFG_TUD_ENDPOINT0_SIZE,
    .idVendor           = 0x303A,   // Espressif
    .idProduct          = 0x8001,   // pith-hb — distinct from pithddu's 0x4002
    .bcdDevice          = 0x0201,   // bumped: report ID 1 grew a button byte (was 2B axis, now 3B)
    .iManufacturer      = 0x01,
    .iProduct           = 0x02,
    .iSerialNumber      = 0x03,
    .bNumConfigurations = 0x01,
};

// ---- Configuration descriptor: HID only (IN + OUT for the two report ids) ----
static const uint8_t s_config_desc[] = {
    TUD_CONFIG_DESCRIPTOR(1, ITF_NUM_TOTAL, 0, CONFIG_TOTAL_LEN, 0x00, 100),
    TUD_HID_INOUT_DESCRIPTOR(ITF_NUM_HID, STRID_HID, HID_ITF_PROTOCOL_NONE,
                             sizeof(s_hid_report), EPNUM_HID_OUT, EPNUM_HID_IN, 64, 1),
};

// ---- String descriptors ----
static char s_serial[24] = "PITHHB-0000";
static const char *s_strings[] = {
    (const char[]){ 0x09, 0x04 },   // 0: English (0x0409)
    "Pith",                          // 1: Manufacturer
    "Handbrake",                     // 2: Product ("Pith" + "Handbrake" reads
                                      //    cleanly wherever the OS concatenates them)
    s_serial,                        // 3: Serial (filled at init)
    "Pith Handbrake",                // 4: HID interface
};
static uint16_t s_str_desc[32];     // UTF-16LE scratch for tud_descriptor_string_cb

// ---- TinyUSB device descriptor callbacks ----
uint8_t const *tud_descriptor_device_cb(void) { return (const uint8_t *)&s_device_desc; }

uint8_t const *tud_descriptor_configuration_cb(uint8_t index) {
    (void)index;
    return s_config_desc;
}

uint16_t const *tud_descriptor_string_cb(uint8_t index, uint16_t langid) {
    (void)langid;
    uint8_t chr_count;
    if (index == 0) {
        memcpy(&s_str_desc[1], s_strings[0], 2);
        chr_count = 1;
    } else {
        if (index >= sizeof(s_strings) / sizeof(s_strings[0])) return NULL;
        const char *str = s_strings[index];
        chr_count = (uint8_t)strlen(str);
        if (chr_count > 31) chr_count = 31;
        for (uint8_t i = 0; i < chr_count; i++) s_str_desc[1 + i] = str[i];
    }
    // first byte: length (2*chr_count + 2), second byte: string descriptor type
    s_str_desc[0] = (uint16_t)((TUSB_DESC_STRING << 8) | (2 * chr_count + 2));
    return s_str_desc;
}

// ---- HID class callbacks ----
uint8_t const *tud_hid_descriptor_report_cb(uint8_t instance) {
    (void)instance;
    return s_hid_report;
}

// Last axis value sent (report id 1), so a host GET_REPORT poll (DirectInput's
// controller-bind screen does this) returns the real current state instead of
// a stale/zeroed buffer. The button byte is always 0 — no physical button.
static volatile uint16_t s_axis_value = 0;

uint16_t tud_hid_get_report_cb(uint8_t instance, uint8_t report_id,
                               hid_report_type_t report_type,
                               uint8_t *buffer, uint16_t reqlen) {
    (void)instance;
    if (report_id == 1 && report_type == HID_REPORT_TYPE_INPUT) {
        uint8_t rep[3] = { (uint8_t)(s_axis_value & 0xFF), (uint8_t)(s_axis_value >> 8), 0 };
        uint8_t n = reqlen < sizeof(rep) ? (uint8_t)reqlen : (uint8_t)sizeof(rep);
        memcpy(buffer, rep, n);
        return n;
    }
    return 0;
}

// Host -> device on report id 2 (the calibration protocol). For interrupt OUT
// with report IDs, the id is the first payload byte and report_id arrives 0.
void tud_hid_set_report_cb(uint8_t instance, uint8_t report_id,
                           hid_report_type_t report_type,
                           uint8_t const *buffer, uint16_t bufsize) {
    (void)instance; (void)report_type;
    const uint8_t *data = buffer;
    uint16_t len = bufsize;
    uint8_t rid = report_id;
    if (rid == 0 && bufsize > 0) { rid = buffer[0]; data = buffer + 1; len = bufsize - 1; }
    if (rid == 2 && len > 0) pith_on_hid_cmd(data, (int)len);
}

void tud_hid_report_complete_cb(uint8_t instance, uint8_t const *report, uint16_t len) {
    (void)instance; (void)report; (void)len;
    pith_on_hid_tx_complete();
}

// ---- Public shim API ----
bool pith_hb_usb_mounted(void) { return tud_mounted(); }

bool pith_hid_ready(void) { return tud_hid_ready(); }

bool pith_hid_send_axis(uint16_t value) {
    s_axis_value = value;
    uint8_t rep[3] = { (uint8_t)(value & 0xFF), (uint8_t)(value >> 8), 0 };
    return tud_hid_report(1, rep, sizeof(rep));
}

bool pith_hid_send(uint8_t report_id, const void *data, int len) {
    if (len < 0) return false;
    return tud_hid_report(report_id, data, (uint16_t)len);
}

static void tusb_device_task(void *arg) {
    (void)arg;
    while (1) {
        tud_task();   // never returns; services USB events
    }
}

void pith_hb_usb_init(const char *serial) {
    if (serial && serial[0]) {
        strncpy(s_serial, serial, sizeof(s_serial) - 1);
        s_serial[sizeof(s_serial) - 1] = '\0';
    }

    // Bring up the internal USB OTG PHY in device mode (Lolin S2 Mini, bus-powered).
    usb_phy_handle_t phy_hdl;
    usb_phy_config_t phy_conf = {
        .controller = USB_PHY_CTRL_OTG,
        .otg_mode   = USB_OTG_MODE_DEVICE,
        .target     = USB_PHY_TARGET_INT,
    };
    ESP_ERROR_CHECK(usb_new_phy(&phy_conf, &phy_hdl));

    if (!tusb_init()) {
        ESP_LOGE(TAG, "tusb_init failed");
        return;
    }
    // S2 is single-core — plain xTaskCreate (no core pinning).
    xTaskCreate(tusb_device_task, "tinyusb", 4096, NULL, 5, NULL);
    ESP_LOGI(TAG, "USB HID up — axis + command channel (serial %s)", s_serial);
}
