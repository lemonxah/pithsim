# pith_usb: thin C shim over raw TinyUSB. The tusb_config.h in include/ configures
# the TinyUSB device stack; REQUIRES the raw tinyusb component (pulled via this
# component's idf_component.yml), plus usb (USB OTG PHY) and freertos (OSAL/task).
idf_component_register(
    SRCS "pith_usb.c"
    INCLUDE_DIRS "include"
    REQUIRES tinyusb usb freertos esp_driver_gpio
)

# Raw tinyusb compiles its own sources with `#include "tusb_config.h"`, but
# REQUIRES is one-directional so it can't see our include/ dir. Inject it into the
# tinyusb component target (same mechanism esp_tinyusb uses) so it finds our config.
idf_component_get_property(tusb_lib espressif__tinyusb COMPONENT_LIB)
target_include_directories(${tusb_lib} PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/include")
