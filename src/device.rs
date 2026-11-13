//! The virtual joystick itself: a Linux uinput device that Proton/Wine (and
//! therefore RealFlight Evolution) sees as an ordinary USB joystick.

use std::io;
use std::path::PathBuf;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AbsoluteAxisEvent, AttributeSet, BusType, InputEvent, InputId,
    KeyCode, UinputAbsSetup,
};

use crate::sticks::{AXIS_MAX, AXIS_MIN, Sticks};

pub const DEVICE_NAME: &str = "Virtual RC Joystick";

/// pid.codes test VID/PID, so no real controller profile (and no SDL gamepad
/// mapping) is applied to the device by mistake. Test PIDs are not unique:
/// fine for a private test tool, not for anything redistributed.
///
/// PID 0x0001 was the earlier 4-axis, 1-button layout. RealFlight keys its
/// calibration and channel map on VID/PID, so a new PID makes it treat the
/// padded layout as a new controller instead of reusing the old record.
const VENDOR_ID: u16 = 0x1209;
const PRODUCT_ID: u16 = 0x0002;

/// Axis order of an EdgeTX radio (like the TX15) in USB joystick mode with the
/// default AETR channel order: CH1 aileron -> X, CH2 elevator -> Y,
/// CH3 throttle -> Z, CH4 rudder -> RX.
pub const AXES: [AbsoluteAxisCode; 4] = [
    AbsoluteAxisCode::ABS_X,
    AbsoluteAxisCode::ABS_Y,
    AbsoluteAxisCode::ABS_Z,
    AbsoluteAxisCode::ABS_RX,
];

/// Axes that are never moved; they stay centred.
///
/// RealFlight Evolution crashes when calibrating a joystick with only
/// 4 axes and 1 button. These axes and [`SPARE_BUTTONS`] pad the device to
/// 8 axes + 8 buttons, the same shape as a radio's joystick (where they would
/// be switches), which RealFlight calibrates fine. Being after X/Y/Z/RX in
/// evdev code order, they show up as axes 5-8 and do not shift axes 1-4.
pub const SPARE_AXES: [AbsoluteAxisCode; 4] = [
    AbsoluteAxisCode::ABS_RY,
    AbsoluteAxisCode::ABS_RZ,
    AbsoluteAxisCode::ABS_THROTTLE,
    AbsoluteAxisCode::ABS_RUDDER,
];

/// Joystick-style buttons, never pressed. Joystick (not gamepad) codes keep
/// SDL, and so Steam Input, from treating the device as a gamepad and
/// remapping its axes. BTN_TRIGGER also makes udev and SDL classify the
/// device as a joystick.
pub const SPARE_BUTTONS: [KeyCode; 8] = [
    KeyCode::BTN_TRIGGER,
    KeyCode::BTN_THUMB,
    KeyCode::BTN_THUMB2,
    KeyCode::BTN_TOP,
    KeyCode::BTN_TOP2,
    KeyCode::BTN_PINKIE,
    KeyCode::BTN_BASE,
    KeyCode::BTN_BASE2,
];

pub struct VirtualJoystick {
    dev: VirtualDevice,
    last: [i32; 4],
    /// `/dev/input/eventN` node of the device, if it could be found.
    pub node: Option<PathBuf>,
}

impl VirtualJoystick {
    /// Creates the device with throttle at idle and all sticks centred.
    pub fn create() -> io::Result<Self> {
        let initial = Sticks::NEUTRAL.to_axes();
        let mut builder = VirtualDevice::builder()?
            .name(DEVICE_NAME)
            .input_id(InputId::new(BusType::BUS_USB, VENDOR_ID, PRODUCT_ID, 1));
        let spare = SPARE_AXES.into_iter().map(|code| (code, 0));
        for (code, value) in AXES.into_iter().zip(initial).chain(spare) {
            let info = AbsInfo::new(value, AXIS_MIN, AXIS_MAX, 0, 0, 0);
            builder = builder.with_absolute_axis(&UinputAbsSetup::new(code, info))?;
        }
        builder = builder.with_keys(&AttributeSet::from_iter(SPARE_BUTTONS))?;

        let mut dev = builder.build()?;
        let node = dev.enumerate_dev_nodes_blocking()?.find_map(Result::ok);
        Ok(Self { dev, last: initial, node })
    }

    /// Sends the stick positions; does nothing if they have not changed.
    pub fn send(&mut self, sticks: Sticks) -> io::Result<()> {
        let axes = sticks.to_axes();
        if axes == self.last {
            return Ok(());
        }
        let events: Vec<InputEvent> = AXES
            .into_iter()
            .zip(axes)
            .map(|(code, value)| *AbsoluteAxisEvent::new(code, value))
            .collect();
        self.dev.emit(&events)?;
        self.last = axes;
        Ok(())
    }
}
