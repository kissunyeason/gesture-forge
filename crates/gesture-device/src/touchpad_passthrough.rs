use std::{collections::BTreeSet, path::Path};

use anyhow::{bail, Context, Result};
use evdev::{
    uinput::VirtualDevice, AbsoluteAxisCode, Device, EventType, InputEvent as EvdevInputEvent,
    UinputAbsSetup,
};

use crate::RawInputEvent;

const DEVICE_NAME: &str = "GestureForge Virtual Touchpad";

const EVENT_SYNCHRONIZATION: &str = "SYNCHRONIZATION";
const EVENT_KEY: &str = "KEY";
const EVENT_ABSOLUTE: &str = "ABSOLUTE";

const SYN_REPORT: u16 = 0;
const ABS_MT_SLOT: u16 = 47;
const ABS_MT_TRACKING_ID: u16 = 57;

const BTN_LEFT: u16 = 272;
const BTN_TOOL_FINGER: u16 = 325;
const BTN_TOOL_QUINTTAP: u16 = 328;
const BTN_TOUCH: u16 = 330;
const BTN_TOOL_DOUBLETAP: u16 = 333;
const BTN_TOOL_TRIPLETAP: u16 = 334;
const BTN_TOOL_QUADTAP: u16 = 335;

/// Exclusive touchpad proxy that replays one- and two-finger input through a
/// virtual touchpad while withholding three-or-more-finger sessions.
///
/// Physical events are buffered until `SYN_REPORT`, so the complete finger
/// count for a frame is known before anything is exposed to the desktop. When
/// a third finger appears after an already-forwarded one- or two-finger prefix,
/// the proxy first terminates every virtual contact and then suppresses input
/// until all physical fingers have been lifted.
pub struct TouchpadPassthrough {
    device: VirtualDevice,
    router: TouchpadRouter,
}

impl TouchpadPassthrough {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let physical = Device::open(path)
            .with_context(|| format!("failed to inspect touchpad {}", path.display()))?;
        let device = create_virtual_touchpad(&physical)
            .with_context(|| format!("failed to proxy touchpad {}", path.display()))?;
        Ok(Self {
            device,
            router: TouchpadRouter::default(),
        })
    }

    /// Feed one raw physical event. Complete frames are either replayed to the
    /// virtual touchpad or consumed according to the current finger count.
    pub fn push(&mut self, event: &RawInputEvent) -> Result<()> {
        let Some(packet) = self.router.push(event) else {
            return Ok(());
        };

        if !packet.events.is_empty() {
            self.device
                .emit(&packet.events)
                .context("failed to emit virtual touchpad frame")?;
        }
        Ok(())
    }

    /// End every contact currently visible on the virtual touchpad.
    pub fn release_all(&mut self) -> Result<()> {
        let events = self.router.release_packet();
        if events.is_empty() {
            return Ok(());
        }
        self.device
            .emit(&events)
            .context("failed to release virtual touchpad contacts")
    }
}

impl Drop for TouchpadPassthrough {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}

fn create_virtual_touchpad(physical: &Device) -> Result<VirtualDevice> {
    let keys = physical
        .supported_keys()
        .context("selected device does not report touchpad key capabilities")?;
    let abs_info = physical
        .get_absinfo()
        .context("failed to query touchpad absolute-axis metadata")?
        .collect::<Vec<_>>();

    let required_axes = [
        AbsoluteAxisCode::ABS_X,
        AbsoluteAxisCode::ABS_Y,
        AbsoluteAxisCode::ABS_MT_SLOT,
        AbsoluteAxisCode::ABS_MT_POSITION_X,
        AbsoluteAxisCode::ABS_MT_POSITION_Y,
        AbsoluteAxisCode::ABS_MT_TRACKING_ID,
    ];
    for required in required_axes {
        if !abs_info.iter().any(|(axis, _)| *axis == required) {
            bail!("selected device is missing required touchpad axis {required:?}");
        }
    }

    let mut builder = VirtualDevice::builder()
        .context("failed to open /dev/uinput for virtual touchpad")?
        .name(DEVICE_NAME)
        .input_id(physical.input_id())
        .with_keys(keys)
        .context("failed to copy touchpad key capabilities")?
        .with_properties(physical.properties())
        .context("failed to copy touchpad input properties")?;

    for (axis, info) in abs_info {
        builder = builder
            .with_absolute_axis(&UinputAbsSetup::new(axis, info))
            .with_context(|| format!("failed to configure virtual touchpad axis {axis:?}"))?;
    }

    builder
        .build()
        .context("failed to create GestureForge virtual touchpad")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RoutingMode {
    #[default]
    Forwarding,
    Suppressing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteOutcome {
    Forwarded,
    Suppressed,
    ReleasedAndSuppressed,
}

struct RoutedPacket {
    #[allow(dead_code)]
    outcome: RouteOutcome,
    events: Vec<EvdevInputEvent>,
}

#[derive(Default)]
struct TouchpadRouter {
    mode: RoutingMode,
    current_slot: u16,
    physical_slots: BTreeSet<u16>,
    physical_tool_keys: [bool; 6],
    physical_touch_down: bool,
    physical_button_down: bool,
    forwarded_slots: BTreeSet<u16>,
    forwarded_tool_keys: [bool; 6],
    forwarded_touch_down: bool,
    forwarded_button_down: bool,
    frame: Vec<RawInputEvent>,
}

impl TouchpadRouter {
    fn push(&mut self, event: &RawInputEvent) -> Option<RoutedPacket> {
        self.update_physical_state(event);

        if event.event_type == EVENT_SYNCHRONIZATION && event.code == SYN_REPORT {
            return Some(self.finish_frame());
        }

        self.frame.push(event.clone());
        None
    }

    fn update_physical_state(&mut self, event: &RawInputEvent) {
        match event.event_type.as_str() {
            EVENT_ABSOLUTE => match event.code {
                ABS_MT_SLOT if event.value >= 0 => self.current_slot = event.value as u16,
                ABS_MT_TRACKING_ID if event.value < 0 => {
                    self.physical_slots.remove(&self.current_slot);
                }
                ABS_MT_TRACKING_ID => {
                    self.physical_slots.insert(self.current_slot);
                }
                _ => {}
            },
            EVENT_KEY => match event.code {
                BTN_LEFT => self.physical_button_down = event.value != 0,
                BTN_TOUCH => self.physical_touch_down = event.value != 0,
                BTN_TOOL_FINGER => self.physical_tool_keys[1] = event.value != 0,
                BTN_TOOL_DOUBLETAP => self.physical_tool_keys[2] = event.value != 0,
                BTN_TOOL_TRIPLETAP => self.physical_tool_keys[3] = event.value != 0,
                BTN_TOOL_QUADTAP => self.physical_tool_keys[4] = event.value != 0,
                BTN_TOOL_QUINTTAP => self.physical_tool_keys[5] = event.value != 0,
                _ => {}
            },
            _ => {}
        }
    }

    fn finish_frame(&mut self) -> RoutedPacket {
        let fingers = self.physical_fingers();

        let packet = match self.mode {
            RoutingMode::Forwarding if fingers >= 3 => {
                self.mode = RoutingMode::Suppressing;
                RoutedPacket {
                    outcome: RouteOutcome::ReleasedAndSuppressed,
                    events: self.release_packet(),
                }
            }
            RoutingMode::Forwarding => {
                let events = self
                    .frame
                    .iter()
                    .filter_map(raw_to_evdev)
                    .collect::<Vec<_>>();
                self.forwarded_slots = self.physical_slots.clone();
                self.forwarded_tool_keys = self.physical_tool_keys;
                self.forwarded_touch_down = self.physical_touch_down;
                self.forwarded_button_down = self.physical_button_down;
                RoutedPacket {
                    outcome: RouteOutcome::Forwarded,
                    events,
                }
            }
            RoutingMode::Suppressing => {
                if fingers == 0 {
                    self.mode = RoutingMode::Forwarding;
                }
                RoutedPacket {
                    outcome: RouteOutcome::Suppressed,
                    events: Vec::new(),
                }
            }
        };

        self.frame.clear();
        packet
    }

    fn physical_fingers(&self) -> usize {
        let reported = self
            .physical_tool_keys
            .iter()
            .rposition(|active| *active)
            .unwrap_or_default();
        reported.max(self.physical_slots.len())
    }

    fn release_packet(&mut self) -> Vec<EvdevInputEvent> {
        let mut events = Vec::new();

        for slot in self.forwarded_slots.iter().copied() {
            events.push(absolute_event(ABS_MT_SLOT, i32::from(slot)));
            events.push(absolute_event(ABS_MT_TRACKING_ID, -1));
        }
        if self.forwarded_button_down {
            events.push(key_event(BTN_LEFT, 0));
        }
        if self.forwarded_touch_down {
            events.push(key_event(BTN_TOUCH, 0));
        }
        for (fingers, code) in [
            (1, BTN_TOOL_FINGER),
            (2, BTN_TOOL_DOUBLETAP),
            (3, BTN_TOOL_TRIPLETAP),
            (4, BTN_TOOL_QUADTAP),
            (5, BTN_TOOL_QUINTTAP),
        ] {
            if self.forwarded_tool_keys[fingers] {
                events.push(key_event(code, 0));
            }
        }

        self.forwarded_slots.clear();
        self.forwarded_tool_keys = [false; 6];
        self.forwarded_touch_down = false;
        self.forwarded_button_down = false;
        events
    }
}

fn raw_to_evdev(event: &RawInputEvent) -> Option<EvdevInputEvent> {
    let event_type = match event.event_type.as_str() {
        EVENT_KEY => EventType::KEY,
        EVENT_ABSOLUTE => EventType::ABSOLUTE,
        _ => return None,
    };
    Some(EvdevInputEvent::new_now(
        event_type.0,
        event.code,
        event.value,
    ))
}

fn key_event(code: u16, value: i32) -> EvdevInputEvent {
    EvdevInputEvent::new_now(EventType::KEY.0, code, value)
}

fn absolute_event(code: u16, value: i32) -> EvdevInputEvent {
    EvdevInputEvent::new_now(EventType::ABSOLUTE.0, code, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event_type: &str, code: u16, value: i32) -> RawInputEvent {
        RawInputEvent {
            timestamp_micros: Some(1),
            event_type: event_type.to_owned(),
            code,
            value,
            summary: String::new(),
        }
    }

    fn absolute(code: u16, value: i32) -> RawInputEvent {
        event(EVENT_ABSOLUTE, code, value)
    }

    fn key(code: u16, value: i32) -> RawInputEvent {
        event(EVENT_KEY, code, value)
    }

    fn sync() -> RawInputEvent {
        event(EVENT_SYNCHRONIZATION, SYN_REPORT, 0)
    }

    fn begin_contact(router: &mut TouchpadRouter, slot: u16, tracking_id: i32) {
        assert!(router
            .push(&absolute(ABS_MT_SLOT, i32::from(slot)))
            .is_none());
        assert!(router
            .push(&absolute(ABS_MT_TRACKING_ID, tracking_id))
            .is_none());
    }

    #[test]
    fn forwards_one_and_two_finger_frames() {
        let mut router = TouchpadRouter::default();
        begin_contact(&mut router, 0, 10);
        router.push(&key(BTN_TOUCH, 1));
        router.push(&key(BTN_TOOL_FINGER, 1));
        let first = router.push(&sync()).unwrap();
        assert_eq!(first.outcome, RouteOutcome::Forwarded);
        assert!(!first.events.is_empty());

        router.push(&key(BTN_TOOL_FINGER, 0));
        begin_contact(&mut router, 1, 11);
        router.push(&key(BTN_TOOL_DOUBLETAP, 1));
        let second = router.push(&sync()).unwrap();
        assert_eq!(second.outcome, RouteOutcome::Forwarded);
        assert!(!second.events.is_empty());
    }

    #[test]
    fn third_finger_releases_forwarded_contacts_and_enters_suppression() {
        let mut router = TouchpadRouter::default();
        begin_contact(&mut router, 0, 10);
        begin_contact(&mut router, 1, 11);
        router.push(&key(BTN_TOUCH, 1));
        router.push(&key(BTN_TOOL_DOUBLETAP, 1));
        assert_eq!(
            router.push(&sync()).unwrap().outcome,
            RouteOutcome::Forwarded
        );

        router.push(&key(BTN_TOOL_DOUBLETAP, 0));
        begin_contact(&mut router, 2, 12);
        router.push(&key(BTN_TOOL_TRIPLETAP, 1));
        let third = router.push(&sync()).unwrap();

        assert_eq!(third.outcome, RouteOutcome::ReleasedAndSuppressed);
        assert_eq!(router.mode, RoutingMode::Suppressing);
        assert_eq!(
            third
                .events
                .iter()
                .filter(|event| {
                    event.event_type() == EventType::ABSOLUTE
                        && event.code() == ABS_MT_TRACKING_ID
                        && event.value() == -1
                })
                .count(),
            2
        );
        assert!(third.events.iter().any(|event| {
            event.event_type() == EventType::KEY
                && event.code() == BTN_TOOL_DOUBLETAP
                && event.value() == 0
        }));
        assert!(!third.events.iter().any(|event| {
            event.event_type() == EventType::KEY
                && event.code() == BTN_TOOL_TRIPLETAP
                && event.value() == 1
        }));
    }

    #[test]
    fn suppression_continues_until_all_fingers_are_lifted() {
        let mut router = TouchpadRouter {
            mode: RoutingMode::Suppressing,
            ..TouchpadRouter::default()
        };
        begin_contact(&mut router, 0, 10);
        router.push(&key(BTN_TOOL_FINGER, 1));
        assert_eq!(
            router.push(&sync()).unwrap().outcome,
            RouteOutcome::Suppressed
        );
        assert_eq!(router.mode, RoutingMode::Suppressing);

        router.push(&absolute(ABS_MT_SLOT, 0));
        router.push(&absolute(ABS_MT_TRACKING_ID, -1));
        router.push(&key(BTN_TOOL_FINGER, 0));
        assert_eq!(
            router.push(&sync()).unwrap().outcome,
            RouteOutcome::Suppressed
        );
        assert_eq!(router.mode, RoutingMode::Forwarding);

        begin_contact(&mut router, 0, 20);
        router.push(&key(BTN_TOUCH, 1));
        router.push(&key(BTN_TOOL_FINGER, 1));
        assert_eq!(
            router.push(&sync()).unwrap().outcome,
            RouteOutcome::Forwarded
        );
    }

    #[test]
    fn release_packet_is_idempotent() {
        let mut router = TouchpadRouter::default();
        router.forwarded_slots.extend([0, 1]);
        router.forwarded_tool_keys[2] = true;
        router.forwarded_touch_down = true;
        router.forwarded_button_down = true;

        let first = router.release_packet();
        assert!(!first.is_empty());
        assert!(router.release_packet().is_empty());
    }
}
