//! A virtual pen tablet and touchscreen through uinput. libinput sees a screen tablet
//! (`INPUT_PROP_DIRECT`, like a Cintiq), so every compositor exposes it as a
//! regular tablet with pressure, tilt and eraser. Mapping it onto the virtual
//! monitor is the desktop's usual "map tablet to monitor" setting.

use anyhow::{Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode,
    PropType, UinputAbsSetup,
};
use tabula_protocol::{Contact, Pen, Tool, buttons};

/// Names the devices show up under; desktops remember the monitor mapping by them.
pub const DEVICE_NAME: &str = "tabula pen";
pub const TOUCH_DEVICE_NAME: &str = "tabula touch";

/// pid.codes test VID/PID for open source prototypes.
const VENDOR: u16 = 0x1209;
const PRODUCT: u16 = 0x0001;
const TOUCH_PRODUCT: u16 = 0x0002;
/// Simultaneous fingers the touchscreen reports.
const MAX_SLOTS: usize = 10;

/// Positions arrive normalized to 0..=65535 and are passed through unchanged,
/// so the axis range doesn't depend on the stream resolution.
const POS_MAX: i32 = 65535;
const PRESSURE_MAX: i32 = 4095;
/// Kernel convention: ABS_TILT_* resolution is in units per radian.
const TILT_RES: i32 = 57;

/// Physical size of the tablet's drawing area, from its `Hello`.
#[derive(Debug, Clone, Copy)]
pub struct PhysicalSize {
    pub width_mm: f32,
    pub height_mm: f32,
}

impl PhysicalSize {
    pub fn from_pixels(width_px: u16, height_px: u16, dpi: u16) -> Self {
        let mm = |px: u16| px as f32 / dpi.max(1) as f32 * 25.4;
        Self { width_mm: mm(width_px).max(1.0), height_mm: mm(height_px).max(1.0) }
    }
}

pub struct PenDevice {
    device: VirtualDevice,
    state: PenState,
}

impl PenDevice {
    pub fn new(size: PhysicalSize) -> Result<Self> {
        let keys = AttributeSet::from_iter([
            KeyCode::BTN_TOOL_PEN,
            KeyCode::BTN_TOOL_RUBBER,
            KeyCode::BTN_TOUCH,
            KeyCode::BTN_STYLUS,
            KeyCode::BTN_STYLUS2,
        ]);
        // Resolution in units per mm; libinput needs it to report physical sizes.
        let res_x = (POS_MAX as f32 / size.width_mm).round().max(1.0) as i32;
        let res_y = (POS_MAX as f32 / size.height_mm).round().max(1.0) as i32;
        let axis = |code, min, max, res| UinputAbsSetup::new(code, AbsInfo::new(0, min, max, 0, 0, res));
        let device = VirtualDevice::builder()
            .context("opening /dev/uinput (run `sudo tabula setup` to allow access)")?
            .name(DEVICE_NAME)
            .input_id(InputId::new(BusType::BUS_VIRTUAL, VENDOR, PRODUCT, 1))
            .with_properties(&AttributeSet::from_iter([PropType::DIRECT]))?
            .with_keys(&keys)?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_X, 0, POS_MAX, res_x))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_Y, 0, POS_MAX, res_y))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_PRESSURE, 0, PRESSURE_MAX, 0))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_TILT_X, -90, 90, TILT_RES))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_TILT_Y, -90, 90, TILT_RES))?
            .build()
            .context("creating uinput pen device")?;
        tracing::info!(name = DEVICE_NAME, ?size, "virtual pen created");
        Ok(Self { device, state: PenState::default() })
    }

    pub fn pen(&mut self, pen: &Pen) -> Result<()> {
        for frame in self.state.update(pen) {
            self.device.emit(&frame).context("writing pen events")?;
        }
        Ok(())
    }

    /// Takes the pen out of proximity, e.g. when the client disconnects.
    pub fn release(&mut self) -> Result<()> {
        for frame in self.state.leave() {
            self.device.emit(&frame).context("writing pen events")?;
        }
        Ok(())
    }
}

impl Drop for PenDevice {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// Turns protocol pen samples into evdev event frames (each frame is
/// followed by SYN_REPORT when emitted).
#[derive(Debug, Default)]
struct PenState {
    /// Tool currently in proximity.
    tool: Option<Tool>,
}

fn key(code: KeyCode, down: bool) -> InputEvent {
    InputEvent::new(EventType::KEY.0, code.0, down as i32)
}

fn abs(code: AbsoluteAxisCode, value: i32) -> InputEvent {
    InputEvent::new(EventType::ABSOLUTE.0, code.0, value)
}

fn tool_key(tool: Tool) -> KeyCode {
    match tool {
        Tool::Pen => KeyCode::BTN_TOOL_PEN,
        Tool::Eraser => KeyCode::BTN_TOOL_RUBBER,
    }
}

impl PenState {
    fn update(&mut self, pen: &Pen) -> Vec<Vec<InputEvent>> {
        if !pen.in_range {
            return self.leave();
        }
        let mut frames = Vec::new();
        // Switching tools (pen flipped to its eraser end) needs the old
        // tool to leave proximity first.
        if self.tool.is_some_and(|t| t != pen.tool) {
            frames.extend(self.leave());
        }
        let pressure = if pen.contact {
            ((pen.pressure as u32 * PRESSURE_MAX as u32 + 32767) / 65535).max(1) as i32
        } else {
            0
        };
        frames.push(vec![
            abs(AbsoluteAxisCode::ABS_X, pen.x as i32),
            abs(AbsoluteAxisCode::ABS_Y, pen.y as i32),
            abs(AbsoluteAxisCode::ABS_PRESSURE, pressure),
            abs(AbsoluteAxisCode::ABS_TILT_X, (pen.tilt_x as i32).clamp(-90, 90)),
            abs(AbsoluteAxisCode::ABS_TILT_Y, (pen.tilt_y as i32).clamp(-90, 90)),
            key(tool_key(pen.tool), true),
            key(KeyCode::BTN_TOUCH, pen.contact),
            key(KeyCode::BTN_STYLUS, pen.buttons & buttons::BARREL != 0),
            key(KeyCode::BTN_STYLUS2, pen.buttons & buttons::BARREL2 != 0),
        ]);
        self.tool = Some(pen.tool);
        frames
    }

    fn leave(&mut self) -> Vec<Vec<InputEvent>> {
        let Some(tool) = self.tool.take() else { return vec![] };
        vec![vec![
            abs(AbsoluteAxisCode::ABS_PRESSURE, 0),
            key(KeyCode::BTN_TOUCH, false),
            key(KeyCode::BTN_STYLUS, false),
            key(KeyCode::BTN_STYLUS2, false),
            key(tool_key(tool), false),
        ]]
    }
}

/// Physical size in units per mm for a normalized 0..=65535 axis.
fn resolution(size: PhysicalSize) -> (i32, i32) {
    let res = |mm: f32| (POS_MAX as f32 / mm).round().max(1.0) as i32;
    (res(size.width_mm), res(size.height_mm))
}

pub struct TouchDevice {
    device: VirtualDevice,
    state: TouchState,
}

impl TouchDevice {
    pub fn new(size: PhysicalSize) -> Result<Self> {
        let (res_x, res_y) = resolution(size);
        let axis = |code, min, max, res| UinputAbsSetup::new(code, AbsInfo::new(0, min, max, 0, 0, res));
        let device = VirtualDevice::builder()
            .context("opening /dev/uinput (run `sudo tabula setup` to allow access)")?
            .name(TOUCH_DEVICE_NAME)
            .input_id(InputId::new(BusType::BUS_VIRTUAL, VENDOR, TOUCH_PRODUCT, 1))
            .with_properties(&AttributeSet::from_iter([PropType::DIRECT]))?
            .with_keys(&AttributeSet::from_iter([KeyCode::BTN_TOUCH]))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_X, 0, POS_MAX, res_x))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_Y, 0, POS_MAX, res_y))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_MT_SLOT, 0, MAX_SLOTS as i32 - 1, 0))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_MT_TRACKING_ID, 0, 65535, 0))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_MT_POSITION_X, 0, POS_MAX, res_x))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_MT_POSITION_Y, 0, POS_MAX, res_y))?
            .build()
            .context("creating uinput touch device")?;
        tracing::info!(name = TOUCH_DEVICE_NAME, "virtual touchscreen created");
        Ok(Self { device, state: TouchState::default() })
    }

    pub fn touch(&mut self, contacts: &[Contact]) -> Result<()> {
        if let Some(frame) = self.state.update(contacts) {
            self.device.emit(&frame).context("writing touch events")?;
        }
        Ok(())
    }

    pub fn release(&mut self) -> Result<()> {
        self.touch(&[])
    }
}

impl Drop for TouchDevice {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// Maps protocol contacts onto multitouch type B slots.
#[derive(Debug, Default)]
struct TouchState {
    /// Client contact id held by each slot.
    slots: [Option<u8>; MAX_SLOTS],
    next_tracking_id: u16,
}

impl TouchState {
    /// One event frame for the new contact set, or `None` if nothing changed.
    fn update(&mut self, contacts: &[Contact]) -> Option<Vec<InputEvent>> {
        let mut events = Vec::new();
        let mut current = None;
        let mut select = |events: &mut Vec<InputEvent>, slot: usize| {
            if current != Some(slot) {
                events.push(abs(AbsoluteAxisCode::ABS_MT_SLOT, slot as i32));
                current = Some(slot);
            }
        };
        // Lifted fingers.
        for slot in 0..MAX_SLOTS {
            if let Some(id) = self.slots[slot]
                && !contacts.iter().any(|c| c.id == id)
            {
                select(&mut events, slot);
                events.push(abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, -1));
                self.slots[slot] = None;
            }
        }
        // Moved and new fingers; fingers beyond MAX_SLOTS are ignored.
        for c in contacts {
            match self.slots.iter().position(|s| *s == Some(c.id)) {
                Some(slot) => select(&mut events, slot),
                None => {
                    let Some(slot) = self.slots.iter().position(Option::is_none) else { continue };
                    self.slots[slot] = Some(c.id);
                    select(&mut events, slot);
                    events.push(abs(AbsoluteAxisCode::ABS_MT_TRACKING_ID, self.next_tracking_id as i32));
                    self.next_tracking_id = (self.next_tracking_id + 1) % 65535;
                }
            }
            events.push(abs(AbsoluteAxisCode::ABS_MT_POSITION_X, c.x as i32));
            events.push(abs(AbsoluteAxisCode::ABS_MT_POSITION_Y, c.y as i32));
        }
        if events.is_empty() {
            return None;
        }
        // Single-touch emulation for clients that don't read MT axes: follow
        // the finger in the lowest occupied slot.
        let first = self
            .slots
            .iter()
            .flatten()
            .next()
            .and_then(|id| contacts.iter().find(|c| c.id == *id));
        events.push(key(KeyCode::BTN_TOUCH, first.is_some()));
        if let Some(c) = first {
            events.push(abs(AbsoluteAxisCode::ABS_X, c.x as i32));
            events.push(abs(AbsoluteAxisCode::ABS_Y, c.y as i32));
        }
        Some(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pen(tool: Tool, contact: bool, in_range: bool) -> Pen {
        Pen {
            x: 100,
            y: 200,
            pressure: 65535,
            tilt_x: 10,
            tilt_y: -10,
            tool,
            buttons: 0,
            contact,
            in_range,
        }
    }

    fn value(frame: &[InputEvent], ty: EventType, code: u16) -> Option<i32> {
        frame.iter().find(|e| e.event_type() == ty && e.code() == code).map(|e| e.value())
    }

    #[test]
    fn hover_then_draw_then_leave() {
        let mut s = PenState::default();
        let hover = s.update(&pen(Tool::Pen, false, true));
        assert_eq!(hover.len(), 1);
        assert_eq!(value(&hover[0], EventType::KEY, KeyCode::BTN_TOOL_PEN.0), Some(1));
        assert_eq!(value(&hover[0], EventType::KEY, KeyCode::BTN_TOUCH.0), Some(0));
        assert_eq!(value(&hover[0], EventType::ABSOLUTE, AbsoluteAxisCode::ABS_PRESSURE.0), Some(0));

        let draw = s.update(&pen(Tool::Pen, true, true));
        assert_eq!(value(&draw[0], EventType::KEY, KeyCode::BTN_TOUCH.0), Some(1));
        assert_eq!(value(&draw[0], EventType::ABSOLUTE, AbsoluteAxisCode::ABS_PRESSURE.0), Some(PRESSURE_MAX));

        let leave = s.update(&pen(Tool::Pen, false, false));
        assert_eq!(value(&leave[0], EventType::KEY, KeyCode::BTN_TOOL_PEN.0), Some(0));
        assert!(s.update(&pen(Tool::Pen, false, false)).is_empty(), "leaving twice emits nothing");
    }

    #[test]
    fn tool_switch_leaves_proximity_first() {
        let mut s = PenState::default();
        s.update(&pen(Tool::Pen, true, true));
        let frames = s.update(&pen(Tool::Eraser, true, true));
        assert_eq!(frames.len(), 2);
        assert_eq!(value(&frames[0], EventType::KEY, KeyCode::BTN_TOOL_PEN.0), Some(0));
        assert_eq!(value(&frames[1], EventType::KEY, KeyCode::BTN_TOOL_RUBBER.0), Some(1));
    }

    #[test]
    fn light_touch_keeps_nonzero_pressure() {
        let mut s = PenState::default();
        let mut p = pen(Tool::Pen, true, true);
        p.pressure = 1;
        let f = s.update(&p);
        assert_eq!(value(&f[0], EventType::ABSOLUTE, AbsoluteAxisCode::ABS_PRESSURE.0), Some(1));
    }

    fn contact(id: u8, x: u16) -> Contact {
        Contact { id, x, y: 500 }
    }

    fn values(frame: &[InputEvent], code: AbsoluteAxisCode) -> Vec<i32> {
        frame
            .iter()
            .filter(|e| e.event_type() == EventType::ABSOLUTE && e.code() == code.0)
            .map(|e| e.value())
            .collect()
    }

    #[test]
    fn two_fingers_down_move_and_lift() {
        let mut t = TouchState::default();
        let down = t.update(&[contact(3, 100)]).unwrap();
        assert_eq!(values(&down, AbsoluteAxisCode::ABS_MT_SLOT), vec![0]);
        assert_eq!(values(&down, AbsoluteAxisCode::ABS_MT_TRACKING_ID), vec![0]);
        assert_eq!(value(&down, EventType::KEY, KeyCode::BTN_TOUCH.0), Some(1));

        let second = t.update(&[contact(3, 110), contact(9, 900)]).unwrap();
        assert_eq!(values(&second, AbsoluteAxisCode::ABS_MT_SLOT), vec![0, 1]);
        assert_eq!(values(&second, AbsoluteAxisCode::ABS_MT_TRACKING_ID), vec![1]);
        assert_eq!(values(&second, AbsoluteAxisCode::ABS_MT_POSITION_X), vec![110, 900]);

        // First finger lifts: its slot ends, single-touch emulation follows the other.
        let lift = t.update(&[contact(9, 905)]).unwrap();
        assert_eq!(values(&lift, AbsoluteAxisCode::ABS_MT_TRACKING_ID), vec![-1]);
        assert_eq!(values(&lift, AbsoluteAxisCode::ABS_X), vec![905]);
        assert_eq!(value(&lift, EventType::KEY, KeyCode::BTN_TOUCH.0), Some(1));

        let up = t.update(&[]).unwrap();
        assert_eq!(values(&up, AbsoluteAxisCode::ABS_MT_TRACKING_ID), vec![-1]);
        assert_eq!(value(&up, EventType::KEY, KeyCode::BTN_TOUCH.0), Some(0));
        assert!(t.update(&[]).is_none(), "no contacts twice emits nothing");
    }

    #[test]
    fn extra_fingers_beyond_slots_are_ignored() {
        let mut t = TouchState::default();
        let many: Vec<_> = (0..12).map(|i| contact(i, i as u16)).collect();
        let f = t.update(&many).unwrap();
        assert_eq!(values(&f, AbsoluteAxisCode::ABS_MT_TRACKING_ID).len(), MAX_SLOTS);
    }

    #[test]
    fn physical_size_from_dpi() {
        let s = PhysicalSize::from_pixels(2560, 1600, 254);
        assert!((s.width_mm - 256.0).abs() < 0.1);
        assert!((s.height_mm - 160.0).abs() < 0.1);
    }
}
