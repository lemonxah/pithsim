# pith_msc: thin C shim over raw TinyUSB exposing ONE read-only mass-storage LUN
# backed by a RAM disk the Rust side builds (a synthetic FAT image of the NVS
# blobs). Mirrors the main firmware's pith_usb component structure.
idf_component_register(
    SRCS "pith_msc.c"
    INCLUDE_DIRS "include"
    REQUIRES tinyusb usb freertos esp_driver_gpio
)

# Raw tinyusb compiles its own sources with `#include "tusb_config.h"`, but
# REQUIRES is one-directional so it can't see our include/ dir. Inject it into the
# tinyusb component target (same mechanism esp_tinyusb uses) so it finds our config.
idf_component_get_property(tusb_lib espressif__tinyusb COMPONENT_LIB)
target_include_directories(${tusb_lib} PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/include")
