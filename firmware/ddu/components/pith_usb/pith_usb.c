// pith_usb — thin C shim over raw TinyUSB. See pith_usb.h for rationale.
// Descriptors mirror the legacy main/usb_descriptors.c so the dashboard's
// expectations (VID/PID, CDC + HID-inout, report ids) are unchanged.

#include "pith_usb.h"

#include <string.h>
#include "tusb.h"
#include "class/hid/hid_device.h"
#include "esp_private/usb_phy.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_log.h"

static const char *TAG = "pith_usb";

// Implemented in Rust (src/usb.rs), declared here (not in the bindgen header).
extern void pith_on_hid_cmd(const uint8_t *buf, int len);
extern void pith_on_hid_tx_complete(void);

// ---- HID report descriptor (verbatim from the legacy firmware) ----
// Report ID 1: 32-button joystick. Report ID 2: vendor IN/OUT (63-byte) command
// channel. Report ID 3: vendor IN (63-byte) device->host log/text stream.
static const uint8_t s_hid_report[] = {
    // --- Joystick, Report ID 1 ---
    0x05, 0x01,        // Usage Page (Generic Desktop)
    0x09, 0x04,        // Usage (Joystick)
    0xA1, 0x01,        // Collection (Application)
    0x85, 0x01,        //   Report ID (1)
    0x05, 0x09,        //   Usage Page (Button)
    0x19, 0x01,        //   Usage Minimum (Button 1)
    0x29, 0x20,        //   Usage Maximum (Button 32)
    0x15, 0x00,        //   Logical Minimum (0)
    0x25, 0x01,        //   Logical Maximum (1)
    0x75, 0x01,        //   Report Size (1)
    0x95, 0x20,        //   Report Count (32)
    0x81, 0x02,        //   Input (Data,Var,Abs)
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
    0xC0,              // End Collection
    // --- Log channel, Report ID 3 (device -> host text only) ---
    0x06, 0x00, 0xFF,  // Usage Page (Vendor-defined 0xFF00)
    0x09, 0x02,        // Usage (0x02)
    0xA1, 0x01,        // Collection (Application)
    0x85, 0x03,        //   Report ID (3)
    0x15, 0x00,        //   Logical Minimum (0)
    0x26, 0xFF, 0x00,  //   Logical Maximum (255)
    0x75, 0x08,        //   Report Size (8)
    0x95, 0x3F,        //   Report Count (63)
    0x09, 0x02, 0x81, 0x02,  //   Input (Data,Var,Abs)  device -> host
    0xC0               // End Collection
};

// ---- Interface / endpoint / string-index layout ----
enum {
    ITF_NUM_CDC = 0,    // CDC notification interface
    ITF_NUM_CDC_DATA,   // CDC data interface
    ITF_NUM_HID,        // HID gamepad interface
    ITF_NUM_TOTAL
};
#define EPNUM_CDC_NOTIF 0x81
#define EPNUM_CDC_OUT   0x02
#define EPNUM_CDC_IN    0x82
#define EPNUM_HID_OUT   0x04    // host -> device command reports
#define EPNUM_HID_IN    0x83    // device -> host (joystick + replies)
#define STRID_CDC  4
#define STRID_HID  5
#define CONFIG_TOTAL_LEN (TUD_CONFIG_DESC_LEN + TUD_CDC_DESC_LEN + TUD_HID_INOUT_DESC_LEN)

// ---- Device descriptor (IAD-class so CDC + HID coexist on Windows) ----
static const tusb_desc_device_t s_device_desc = {
    .bLength            = sizeof(tusb_desc_device_t),
    .bDescriptorType    = TUSB_DESC_DEVICE,
    .bcdUSB             = 0x0200,
    .bDeviceClass       = TUSB_CLASS_MISC,
    .bDeviceSubClass    = MISC_SUBCLASS_COMMON,
    .bDeviceProtocol    = MISC_PROTOCOL_IAD,
    .bMaxPacketSize0    = CFG_TUD_ENDPOINT0_SIZE,
    .idVendor           = 0x303A,   // Espressif
    .idProduct          = 0x4002,
    // v2 (bumped from 0x0100): the HID descriptor changed from Gamepad -> Joystick,
    // and Steam caches its controller profile by device identity (VID:PID:version).
    // Bumping the version changes the SDL GUID so Steam re-detects it as a fresh
    // joystick instead of applying the stale gamepad mapping (Guide -> Big Picture).
    .bcdDevice          = 0x0200,
    .iManufacturer      = 0x01,
    .iProduct           = 0x02,
    .iSerialNumber      = 0x03,
    .bNumConfigurations = 0x01,
};

// ---- Configuration descriptor: CDC + HID ----
static const uint8_t s_config_desc[] = {
    TUD_CONFIG_DESCRIPTOR(1, ITF_NUM_TOTAL, 0, CONFIG_TOTAL_LEN, 0x00, 100),
    // CDC: notif EP, then data OUT/IN
    TUD_CDC_DESCRIPTOR(ITF_NUM_CDC, STRID_CDC, EPNUM_CDC_NOTIF, 8,
                       EPNUM_CDC_OUT, EPNUM_CDC_IN, 64),
    // HID: IN (joystick + replies) + OUT (commands), 1 ms poll for low latency
    TUD_HID_INOUT_DESCRIPTOR(ITF_NUM_HID, STRID_HID, HID_ITF_PROTOCOL_NONE,
                             sizeof(s_hid_report), EPNUM_HID_OUT, EPNUM_HID_IN, 64, 1),
};

// ---- String descriptors ----
static char s_serial[24] = "PITH-0000";
static const char *s_strings[] = {
    (const char[]){ 0x09, 0x04 },   // 0: English (0x0409)
    "Pith",                          // 1: Manufacturer
    "Sim Dashboard",                 // 2: Product
    s_serial,                        // 3: Serial (filled at init)
    "Pith Serial",                   // 4: CDC interface
    "Sim Dashboard HID",             // 5: HID interface
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

// Last joystick (report id 1) button mask we sent, so a host GET_REPORT poll
// returns the real current state. DirectInput polls this on the controller bind
// screen; returning 0 here made it read a stale buffer → a phantom stuck button.
static volatile uint32_t s_joy_mask = 0;

uint16_t tud_hid_get_report_cb(uint8_t instance, uint8_t report_id,
                               hid_report_type_t report_type,
                               uint8_t *buffer, uint16_t reqlen) {
    (void)instance;
    // Joystick input report: answer with the live 4-byte button mask (LE).
    if (report_id == 1 && report_type == HID_REPORT_TYPE_INPUT) {
        uint8_t n = reqlen < 4 ? (uint8_t)reqlen : 4;
        uint32_t m = s_joy_mask;
        for (uint8_t i = 0; i < n; i++) buffer[i] = (uint8_t)(m >> (8 * i));
        return n;
    }
    return 0;
}

// Host -> device. The PC app sends command data on report id 2. For interrupt
// OUT with report IDs the id is the first payload byte and report_id arrives 0.
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
bool pith_usb_mounted(void) { return tud_mounted(); }

int pith_cdc_read(uint8_t *buf, int max) {
    if (max <= 0 || !tud_cdc_n_available(0)) return 0;
    return (int)tud_cdc_n_read(0, buf, (uint32_t)max);
}

int pith_cdc_write(const uint8_t *buf, int len) {
    if (len <= 0) return 0;
    return (int)tud_cdc_n_write(0, buf, (uint32_t)len);
}

void pith_cdc_flush(void) { tud_cdc_n_write_flush(0); }

bool pith_hid_ready(void) { return tud_hid_ready(); }

bool pith_hid_send(uint8_t report_id, const void *data, int len) {
    if (len < 0) return false;
    // Cache the joystick mask so GET_REPORT can answer with the current state.
    if (report_id == 1 && len >= 4 && data) {
        const uint8_t *b = (const uint8_t *)data;
        s_joy_mask = (uint32_t)b[0] | ((uint32_t)b[1] << 8) |
                     ((uint32_t)b[2] << 16) | ((uint32_t)b[3] << 24);
    }
    return tud_hid_report(report_id, data, (uint16_t)len);
}

static void tusb_device_task(void *arg) {
    (void)arg;
    while (1) {
        tud_task();   // never returns; services USB events
    }
}

void pith_usb_init(const char *serial) {
    if (serial && serial[0]) {
        strncpy(s_serial, serial, sizeof(s_serial) - 1);
        s_serial[sizeof(s_serial) - 1] = '\0';
    }

    // Bring up the internal USB OTG PHY in device mode (XIAO S3, bus-powered).
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
    // 16 KB stack: the HID/CDC command callbacks run in this task and parse the
    // pushed UiDoc (typed serde deserialization), which overflowed the old 4 KB.
    xTaskCreatePinnedToCore(tusb_device_task, "tinyusb", 16384, NULL, 5, NULL, 0);
    ESP_LOGI(TAG, "USB composite up — CDC + HID (serial %s)", s_serial);
}
