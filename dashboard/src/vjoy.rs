//! Software virtual joystick — the PC-side endpoint for the WiFi axis path.
//! A wireless Pith device (pedal/handbrake) streams its axis value over UDP;
//! the dashboard feeds it into this virtual joystick so the game reads a
//! normal HID gamepad axis, with no USB cable to the device and no bridge
//! dongle. This is the same idea as the reference project's optional vJoy
//! output, done natively per-OS.
//!
//! Linux implementation uses `/dev/uinput` via raw `libc` ioctls (no extra
//! crate). Non-Linux targets get a no-op stub with the same API so the
//! dashboard still builds; a Windows ViGEm/vJoy backend can slot in later
//! behind the same `VirtualJoystick` type.
//!
//! Axis values are the device's native 0..=65535 (the same range the USB HID
//! joystick reports), so a device feels identical whether it's wired or
//! wireless.

/// Up to this many axes on the virtual device (throttle/brake/clutch +
/// handbrake + headroom).
pub const MAX_AXES: usize = 8;
/// Buttons on the virtual device — matches the DDU's 32-button HID "button
/// box" so a wireless DDU's touch buttons map 1:1.
pub const MAX_BUTTONS: usize = 32;

#[cfg(target_os = "linux")]
mod imp {
    use std::io;
    use std::os::fd::RawFd;

    use super::{MAX_AXES, MAX_BUTTONS};

    // --- Linux input/uinput constants (stable kernel ABI) ---
    const EV_SYN: u16 = 0x00;
    const EV_KEY: u16 = 0x01;
    const EV_ABS: u16 = 0x03;
    const SYN_REPORT: u16 = 0x00;
    const BTN_JOYSTICK: u16 = 0x120; // 0x120..=0x12F: the 16 joystick buttons
    const BTN_TRIGGER_HAPPY: u16 = 0x2C0; // 0x2C0…: extra buttons 17..=32
    /// Key code for logical button `i` (0-based): the 16 joystick codes, then
    /// the TRIGGER_HAPPY range — the standard layout for >16-button boxes.
    const fn btn_code(i: usize) -> u16 {
        if i < 16 {
            BTN_JOYSTICK + i as u16
        } else {
            BTN_TRIGGER_HAPPY + (i - 16) as u16
        }
    }
    // ABS axis codes, in the order we assign them to logical axes 0..MAX_AXES.
    const ABS_CODES: [u16; MAX_AXES] = [
        0x00, // ABS_X
        0x01, // ABS_Y
        0x02, // ABS_Z
        0x03, // ABS_RX
        0x04, // ABS_RY
        0x05, // ABS_RZ
        0x06, // ABS_THROTTLE
        0x07, // ABS_RUDDER
    ];
    const ABS_CNT: usize = 0x40; // kernel ABS_CNT
    const UINPUT_MAX_NAME_SIZE: usize = 80;
    const AXIS_MAX: i32 = 65535; // matches the device's u16 axis range

    // ioctl request numbers. _IO('U',n) / _IOW('U',n,size) per asm-generic.
    const UINPUT_IOCTL_BASE: libc::c_ulong = b'U' as libc::c_ulong;
    const fn io(nr: libc::c_ulong) -> libc::c_ulong {
        (UINPUT_IOCTL_BASE << 8) | nr
    }
    const fn iow(nr: libc::c_ulong, size: libc::c_ulong) -> libc::c_ulong {
        (1 << 30) | (size << 16) | (UINPUT_IOCTL_BASE << 8) | nr
    }
    const UI_DEV_CREATE: libc::c_ulong = io(1);
    const UI_DEV_DESTROY: libc::c_ulong = io(2);
    // `int` argument (4 bytes) for the SET_*BIT ioctls.
    fn ui_set_evbit() -> libc::c_ulong {
        iow(100, 4)
    }
    fn ui_set_keybit() -> libc::c_ulong {
        iow(101, 4)
    }
    fn ui_set_absbit() -> libc::c_ulong {
        iow(103, 4)
    }

    #[repr(C)]
    struct InputId {
        bustype: u16,
        vendor: u16,
        product: u16,
        version: u16,
    }

    #[repr(C)]
    struct UinputUserDev {
        name: [libc::c_char; UINPUT_MAX_NAME_SIZE],
        id: InputId,
        ff_effects_max: u32,
        absmax: [i32; ABS_CNT],
        absmin: [i32; ABS_CNT],
        absfuzz: [i32; ABS_CNT],
        absflat: [i32; ABS_CNT],
    }

    #[repr(C)]
    struct InputEvent {
        time: libc::timeval,
        type_: u16,
        code: u16,
        value: i32,
    }

    pub struct VirtualJoystick {
        fd: RawFd,
        axes: usize,
    }

    // The fd is only ever written to (atomic full-event writes); safe to move
    // to the UDP thread.
    unsafe impl Send for VirtualJoystick {}

    impl VirtualJoystick {
        /// Create a `axes`-axis virtual joystick named `name`. `axes` is
        /// clamped to `MAX_AXES`.
        pub fn new(name: &str, axes: usize) -> io::Result<Self> {
            let axes = axes.clamp(1, MAX_AXES);
            let path = c"/dev/uinput";
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let js = VirtualJoystick { fd, axes };
            if let Err(e) = js.configure(name) {
                unsafe { libc::close(fd) };
                return Err(e);
            }
            Ok(js)
        }

        fn ioctl_val(&self, req: libc::c_ulong, val: libc::c_int) -> io::Result<()> {
            if unsafe { libc::ioctl(self.fd, req, val) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        fn configure(&self, name: &str) -> io::Result<()> {
            // Enable the buttons (≥1 required for joystick classification) + axes.
            self.ioctl_val(ui_set_evbit(), EV_KEY as libc::c_int)?;
            for i in 0..MAX_BUTTONS {
                self.ioctl_val(ui_set_keybit(), btn_code(i) as libc::c_int)?;
            }
            self.ioctl_val(ui_set_evbit(), EV_ABS as libc::c_int)?;
            for &code in ABS_CODES.iter().take(self.axes) {
                self.ioctl_val(ui_set_absbit(), code as libc::c_int)?;
            }

            // Legacy device setup: fill uinput_user_dev + UI_DEV_CREATE.
            let mut dev: UinputUserDev = unsafe { std::mem::zeroed() };
            let name_bytes = name.as_bytes();
            let n = name_bytes.len().min(UINPUT_MAX_NAME_SIZE - 1);
            for (dst, &b) in dev.name.iter_mut().zip(name_bytes).take(n) {
                *dst = b as libc::c_char;
            }
            dev.id = InputId {
                bustype: 0x03,   // BUS_USB — games treat it as a normal gamepad
                vendor: 0x303A,  // Espressif VID, matching pith's USB devices
                product: 0x8100, // "pith virtual joystick"
                version: 1,
            };
            for &code in ABS_CODES.iter().take(self.axes) {
                dev.absmin[code as usize] = 0;
                dev.absmax[code as usize] = AXIS_MAX;
            }
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    (&dev as *const UinputUserDev) as *const u8,
                    std::mem::size_of::<UinputUserDev>(),
                )
            };
            let written =
                unsafe { libc::write(self.fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
            if written != bytes.len() as isize {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::ioctl(self.fd, UI_DEV_CREATE) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        fn emit(&self, type_: u16, code: u16, value: i32) -> io::Result<()> {
            let ev = InputEvent {
                time: libc::timeval {
                    tv_sec: 0,
                    tv_usec: 0,
                },
                type_,
                code,
                value,
            };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    (&ev as *const InputEvent) as *const u8,
                    std::mem::size_of::<InputEvent>(),
                )
            };
            let written =
                unsafe { libc::write(self.fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
            if written != bytes.len() as isize {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Set logical `axis` (0-based) to `value` (0..=65535) and flush.
        pub fn set_axis(&self, axis: usize, value: u16) -> io::Result<()> {
            if axis >= self.axes {
                return Ok(());
            }
            self.emit(EV_ABS, ABS_CODES[axis], value as i32)?;
            self.emit(EV_SYN, SYN_REPORT, 0)
        }

        /// Press/release logical button `idx` (0-based, < MAX_BUTTONS) and flush.
        pub fn set_button(&self, idx: usize, pressed: bool) -> io::Result<()> {
            if idx >= MAX_BUTTONS {
                return Ok(());
            }
            self.emit(EV_KEY, btn_code(idx), pressed as i32)?;
            self.emit(EV_SYN, SYN_REPORT, 0)
        }

        /// Axis count (for a future UI axis-mapping view).
        #[allow(dead_code)]
        pub fn axes(&self) -> usize {
            self.axes
        }
    }

    impl Drop for VirtualJoystick {
        fn drop(&mut self) {
            unsafe {
                libc::ioctl(self.fd, UI_DEV_DESTROY);
                libc::close(self.fd);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::io;

    /// No-op stub for non-Linux hosts (a ViGEm/vJoy backend can replace this).
    pub struct VirtualJoystick {
        axes: usize,
    }

    impl VirtualJoystick {
        pub fn new(_name: &str, axes: usize) -> io::Result<Self> {
            Ok(VirtualJoystick {
                axes: axes.clamp(1, super::MAX_AXES),
            })
        }
        pub fn set_axis(&self, _axis: usize, _value: u16) -> io::Result<()> {
            Ok(())
        }
        pub fn set_button(&self, _idx: usize, _pressed: bool) -> io::Result<()> {
            Ok(())
        }
        #[allow(dead_code)]
        pub fn axes(&self) -> usize {
            self.axes
        }
    }
}

pub use imp::VirtualJoystick;

#[cfg(test)]
mod tests {
    use super::*;

    /// Creating + feeding the joystick shouldn't panic. On Linux CI without
    /// permission to /dev/uinput this returns an error (not a panic), which
    /// we tolerate — the point is the API shape and no UB in the ioctl path.
    #[test]
    fn create_and_feed_or_permission_error() {
        match VirtualJoystick::new("pith-test-joystick", 3) {
            Ok(js) => {
                assert_eq!(js.axes(), 3);
                js.set_axis(0, 32768).unwrap();
                js.set_axis(2, 65535).unwrap();
                // out-of-range axis is a no-op, not an error
                js.set_axis(9, 1).unwrap();
            }
            Err(e) => {
                // Acceptable in a sandbox without /dev/uinput access.
                eprintln!("uinput unavailable (expected in sandbox): {e}");
            }
        }
    }
}
