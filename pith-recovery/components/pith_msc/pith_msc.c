// pith_msc — read-only USB mass storage backed by a RAM disk. See pith_msc.h.
// Descriptor/PHY/task structure mirrors the main firmware's pith_usb shim.

#include "pith_msc.h"

#include <string.h>
#include "tusb.h"
#include "esp_private/usb_phy.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_log.h"

static const char *TAG = "pith_msc";

// The RAM disk (set once by pith_msc_start; never freed).
static const uint8_t *s_disk = NULL;
static uint32_t s_disk_size = 0;

// ---- Interface / endpoint layout: a single MSC interface ----
enum { ITF_NUM_MSC = 0, ITF_NUM_TOTAL };
#define EPNUM_MSC_OUT 0x01
#define EPNUM_MSC_IN  0x81
#define CONFIG_TOTAL_LEN (TUD_CONFIG_DESC_LEN + TUD_MSC_DESC_LEN)

// Distinct PID from the dashboard device (0x4002) so the GUI never mistakes the
// recovery drive for a live dashboard and tries to talk HID to it.
static const tusb_desc_device_t s_device_desc = {
    .bLength            = sizeof(tusb_desc_device_t),
    .bDescriptorType    = TUSB_DESC_DEVICE,
    .bcdUSB             = 0x0200,
    .bDeviceClass       = 0x00,
    .bDeviceSubClass    = 0x00,
    .bDeviceProtocol    = 0x00,
    .bMaxPacketSize0    = CFG_TUD_ENDPOINT0_SIZE,
    .idVendor           = 0x303A,   // Espressif
    .idProduct          = 0x4003,
    .bcdDevice          = 0x0100,
    .iManufacturer      = 0x01,
    .iProduct           = 0x02,
    .iSerialNumber      = 0x03,
    .bNumConfigurations = 0x01,
};

static const uint8_t s_config_desc[] = {
    TUD_CONFIG_DESCRIPTOR(1, ITF_NUM_TOTAL, 0, CONFIG_TOTAL_LEN, 0x00, 100),
    TUD_MSC_DESCRIPTOR(ITF_NUM_MSC, 4, EPNUM_MSC_OUT, EPNUM_MSC_IN, 64),
};

static const char *s_strings[] = {
    (const char[]){ 0x09, 0x04 },   // 0: English (0x0409)
    "Pith",                          // 1: Manufacturer
    "Pith DDU Recovery",             // 2: Product
    "PITH-NVS",                      // 3: Serial
    "Pith NVS Storage",              // 4: MSC interface
};
static uint16_t s_str_desc[32];

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
    s_str_desc[0] = (uint16_t)((TUSB_DESC_STRING << 8) | (2 * chr_count + 2));
    return s_str_desc;
}

// ---- MSC class callbacks (single read-only LUN over the RAM disk) ----
void tud_msc_inquiry_cb(uint8_t lun, uint8_t vendor_id[8], uint8_t product_id[16],
                        uint8_t product_rev[4]) {
    (void)lun;
    memcpy(vendor_id,  "Pith    ", 8);
    memcpy(product_id, "NVS Storage     ", 16);
    memcpy(product_rev, "1.0 ", 4);
}

bool tud_msc_test_unit_ready_cb(uint8_t lun) {
    (void)lun;
    return s_disk != NULL;
}

void tud_msc_capacity_cb(uint8_t lun, uint32_t *block_count, uint16_t *block_size) {
    (void)lun;
    *block_count = s_disk_size / 512;
    *block_size  = 512;
}

bool tud_msc_is_writable_cb(uint8_t lun) {
    (void)lun;
    return false; // read-only: the host sees a write-protected drive
}

bool tud_msc_start_stop_cb(uint8_t lun, uint8_t power_condition, bool start, bool load_eject) {
    (void)lun; (void)power_condition; (void)start; (void)load_eject;
    return true;
}

int32_t tud_msc_read10_cb(uint8_t lun, uint32_t lba, uint32_t offset, void *buffer,
                          uint32_t bufsize) {
    (void)lun;
    uint64_t pos = (uint64_t)lba * 512 + offset;
    if (!s_disk || pos >= s_disk_size) return -1;
    uint32_t n = bufsize;
    if (pos + n > s_disk_size) n = (uint32_t)(s_disk_size - pos);
    memcpy(buffer, s_disk + pos, n);
    return (int32_t)n;
}

int32_t tud_msc_write10_cb(uint8_t lun, uint32_t lba, uint32_t offset, uint8_t *buffer,
                           uint32_t bufsize) {
    (void)lun; (void)lba; (void)offset; (void)buffer; (void)bufsize;
    return -1; // write-protected (is_writable_cb already said no)
}

int32_t tud_msc_scsi_cb(uint8_t lun, uint8_t const scsi_cmd[16], void *buffer, uint16_t bufsize) {
    (void)lun; (void)buffer; (void)bufsize;
    switch (scsi_cmd[0]) {
        case SCSI_CMD_PREVENT_ALLOW_MEDIUM_REMOVAL:
            return 0;
        default:
            tud_msc_set_sense(lun, SCSI_SENSE_ILLEGAL_REQUEST, 0x20, 0x00);
            return -1;
    }
}

// ---- Public shim API ----
static void tusb_device_task(void *arg) {
    (void)arg;
    while (1) {
        tud_task();
    }
}

bool pith_msc_start(const uint8_t *disk, uint32_t size) {
    if (!disk || size < 512 || (size % 512) != 0) return false;
    s_disk = disk;
    s_disk_size = size;

    // Bring up the internal USB OTG PHY in device mode (XIAO S3, bus-powered).
    usb_phy_handle_t phy_hdl;
    usb_phy_config_t phy_conf = {
        .controller = USB_PHY_CTRL_OTG,
        .otg_mode   = USB_OTG_MODE_DEVICE,
        .target     = USB_PHY_TARGET_INT,
    };
    if (usb_new_phy(&phy_conf, &phy_hdl) != ESP_OK) {
        ESP_LOGE(TAG, "usb_new_phy failed");
        return false;
    }
    if (!tusb_init()) {
        ESP_LOGE(TAG, "tusb_init failed");
        return false;
    }
    xTaskCreatePinnedToCore(tusb_device_task, "tinyusb", 8192, NULL, 5, NULL, 0);
    ESP_LOGI(TAG, "USB MSC up — %lu KiB read-only NVS disk", (unsigned long)(size / 1024));
    return true;
}
