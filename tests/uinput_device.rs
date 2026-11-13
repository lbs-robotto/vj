//! Creates the real uinput device and checks what a game reads from it.
//!
//! Needs write access to /dev/uinput, so it is ignored by default:
//!     cargo test -- --ignored

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use evdev::Device;
use virtual_joystick::device::{AXES, DEVICE_NAME, SPARE_AXES, SPARE_BUTTONS, VirtualJoystick};
use virtual_joystick::sticks::{ARM_HOLD, AXIS_MAX, AXIS_MIN, Controller, SequenceKind};

/// udev applies the access ACL to a new event node a moment after it appears.
fn open_with_retry(node: &Path) -> Device {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match Device::open(node) {
            Ok(dev) => return dev,
            Err(e) if Instant::now() > deadline => panic!("open {}: {e}", node.display()),
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn read_axes(dev: &Device) -> [i32; 4] {
    let state = dev.get_abs_state().expect("read axis state");
    AXES.map(|code| state[code.0 as usize].value)
}

#[test]
#[ignore = "needs write access to /dev/uinput"]
fn arm_sequence_reaches_the_event_device() {
    let mut joy = VirtualJoystick::create().expect("create uinput device");
    let node = joy.node.clone().expect("event node for the virtual device");
    let dev = open_with_retry(&node);

    assert_eq!(dev.name(), Some(DEVICE_NAME));
    let supported = dev.supported_absolute_axes().expect("device has axes");
    assert!(AXES.iter().chain(&SPARE_AXES).all(|&code| supported.contains(code)));
    assert_eq!(supported.iter().count(), 8, "8 axes, like a radio's joystick");
    let keys = dev.supported_keys().expect("device has buttons");
    assert!(SPARE_BUTTONS.iter().all(|&code| keys.contains(code)));
    assert_eq!(keys.iter().count(), 8, "8 buttons, like a radio's joystick");
    // Starts at throttle idle, sticks centred.
    assert_eq!(read_axes(&dev), [0, 0, AXIS_MIN, 0]);

    let t0 = Instant::now();
    let mut c = Controller::new();
    c.throttle = 0.6;
    c.start(SequenceKind::Arm, t0);

    // During the hold: throttle fully down, yaw fully right.
    joy.send(c.output(t0 + Duration::from_secs(4))).unwrap();
    assert_eq!(read_axes(&dev), [0, 0, AXIS_MIN, AXIS_MAX]);

    // After the hold: yaw back to centre, throttle stays at idle.
    joy.send(c.output(t0 + ARM_HOLD)).unwrap();
    assert_eq!(read_axes(&dev), [0, 0, AXIS_MIN, 0]);

    // Sliders drive throttle (Z) and pitch (Y).
    c.throttle = 1.0;
    c.pitch = 0.5;
    joy.send(c.output(t0 + ARM_HOLD)).unwrap();
    assert_eq!(read_axes(&dev), [0, 16384, AXIS_MAX, 0]);

    // The spare axes never move from centre.
    let state = dev.get_abs_state().expect("read axis state");
    assert!(SPARE_AXES.iter().all(|code| state[code.0 as usize].value == 0));
}
