use std::{collections::BTreeMap, path::Path};

use anyhow::{bail, Context, Result};
use evdev::{
    uinput::VirtualDevice, AbsoluteAxisCode, Device, EventType, InputEvent as EvdevInputEvent,
    UinputAbsSetup,
};

use crate::RawInputEvent;

const DEVICE_NAME: &str = "GestureForge Virtual Touchpad";
const TWO_FINGER_ARBITRATION_MICROS: u128 = 100_000;

const EVENT_SYNCHRONIZATION: &str = "SYNCHRONIZATION";
const EVENT_KEY: &str = "KEY";
const EVENT_ABSOLUTE: &str = "ABSOLUTE";

const SYN_REPORT: u16 = 0;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_MT_SLOT: u16 = 47;
const ABS_MT_POSITION_X: u16 = 53;
const ABS_MT_POSITION_Y: u16 = 54;
const ABS_MT_TOOL_TYPE: u16 = 55;
const ABS_MT_TRACKING_ID: u16 = 57;

const BTN_LEFT: u16 = 272;
const BTN_TOOL_FINGER: u16 = 325;
const BTN_TOOL_QUINTTAP: u16 = 328;
const BTN_TOUCH: u16 = 330;
const BTN_TOOL_DOUBLETAP: u16 = 333;
const BTN_TOOL_TRIPLETAP: u16 = 334;
const BTN_TOOL_QUADTAP: u16 = 335;

/// Exclusive touchpad proxy that synthesizes a new, validated multitouch
/// stream instead of replaying physical protocol-B events verbatim.
///
/// One-finger input is forwarded immediately. A transition to two fingers is
/// held for a short arbitration window so a third finger can claim the session
/// before GNOME sees a two-finger gesture. Three-or-more-finger sessions are
/// suppressed until every physical contact is lifted.
pub struct TouchpadPassthrough {
    device: VirtualDevice,
    router: TouchpadRouter,
    validator: OutputValidator,
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
            validator: OutputValidator::default(),
        })
    }

    /// Feed one raw physical event. Complete physical frames are converted to
    /// independently synthesized and validated virtual frames.
    pub fn push(&mut self, event: &RawInputEvent) -> Result<()> {
        let Some(packet) = self.router.push(event) else {
            return Ok(());
        };

        self.validator
            .validate(&packet.events)
            .context("refusing to emit an invalid virtual touchpad frame")?;
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
        self.validator
            .validate(&events)
            .context("refusing to emit an invalid virtual touchpad release")?;
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
    PendingTwo {
        started_micros: u128,
    },
    Suppressing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteOutcome {
    Forwarded,
    PendingTwo,
    Suppressed,
    ReleasedAndSuppressed,
}

struct RoutedPacket {
    #[allow(dead_code)]
    outcome: RouteOutcome,
    events: Vec<EvdevInputEvent>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PhysicalContact {
    tracking_id: i32,
    x: Option<i32>,
    y: Option<i32>,
    tool_type: Option<i32>,
}

#[derive(Debug, Clone, Default)]
struct PhysicalSnapshot {
    contacts: BTreeMap<u16, PhysicalContact>,
    abs_x: Option<i32>,
    abs_y: Option<i32>,
    button_down: bool,
    reported_fingers: usize,
}

impl PhysicalSnapshot {
    fn fingers(&self) -> usize {
        self.reported_fingers.max(self.contacts.len())
    }

    fn empty() -> Self {
        Self::default()
    }
}

#[derive(Default)]
struct PhysicalState {
    current_slot: u16,
    contacts: BTreeMap<u16, PhysicalContact>,
    abs_x: Option<i32>,
    abs_y: Option<i32>,
    button_down: bool,
    tool_keys: [bool; 6],
    latest_timestamp_micros: Option<u128>,
}

impl PhysicalState {
    fn apply(&mut self, event: &RawInputEvent) {
        self.latest_timestamp_micros = event.timestamp_micros.or(self.latest_timestamp_micros);

        match event.event_type.as_str() {
            EVENT_ABSOLUTE => match event.code {
                ABS_X => self.abs_x = Some(event.value),
                ABS_Y => self.abs_y = Some(event.value),
                ABS_MT_SLOT if event.value >= 0 => self.current_slot = event.value as u16,
                ABS_MT_TRACKING_ID if event.value < 0 => {
                    self.contacts.remove(&self.current_slot);
                }
                ABS_MT_TRACKING_ID => {
                    self.contacts.insert(
                        self.current_slot,
                        PhysicalContact {
                            tracking_id: event.value,
                            ..PhysicalContact::default()
                        },
                    );
                }
                ABS_MT_POSITION_X => {
                    if let Some(contact) = self.contacts.get_mut(&self.current_slot) {
                        contact.x = Some(event.value);
                    }
                }
                ABS_MT_POSITION_Y => {
                    if let Some(contact) = self.contacts.get_mut(&self.current_slot) {
                        contact.y = Some(event.value);
                    }
                }
                ABS_MT_TOOL_TYPE => {
                    if let Some(contact) = self.contacts.get_mut(&self.current_slot) {
                        contact.tool_type = Some(event.value);
                    }
                }
                _ => {}
            },
            EVENT_KEY => match event.code {
                BTN_LEFT => self.button_down = event.value != 0,
                BTN_TOOL_FINGER => self.tool_keys[1] = event.value != 0,
                BTN_TOOL_DOUBLETAP => self.tool_keys[2] = event.value != 0,
                BTN_TOOL_TRIPLETAP => self.tool_keys[3] = event.value != 0,
                BTN_TOOL_QUADTAP => self.tool_keys[4] = event.value != 0,
                BTN_TOOL_QUINTTAP => self.tool_keys[5] = event.value != 0,
                _ => {}
            },
            _ => {}
        }
    }

    fn snapshot(&self) -> PhysicalSnapshot {
        PhysicalSnapshot {
            contacts: self.contacts.clone(),
            abs_x: self.abs_x,
            abs_y: self.abs_y,
            button_down: self.button_down,
            reported_fingers: self
                .tool_keys
                .iter()
                .rposition(|active| *active)
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VirtualContact {
    physical_tracking_id: i32,
    x: Option<i32>,
    y: Option<i32>,
    tool_type: i32,
}

struct VirtualSynthesizer {
    contacts: BTreeMap<u16, VirtualContact>,
    abs_x: Option<i32>,
    abs_y: Option<i32>,
    button_down: bool,
    touch_down: bool,
    tool_fingers: usize,
    next_tracking_id: i32,
}

impl Default for VirtualSynthesizer {
    fn default() -> Self {
        Self {
            contacts: BTreeMap::new(),
            abs_x: None,
            abs_y: None,
            button_down: false,
            touch_down: false,
            tool_fingers: 0,
            next_tracking_id: 1,
        }
    }
}

impl VirtualSynthesizer {
    fn synthesize(&mut self, desired: &PhysicalSnapshot) -> Vec<EvdevInputEvent> {
        let mut events = Vec::new();

        let existing_slots = self.contacts.keys().copied().collect::<Vec<_>>();
        for slot in existing_slots {
            let replace = desired.contacts.get(&slot).is_some_and(|contact| {
                self.contacts
                    .get(&slot)
                    .is_some_and(|current| current.physical_tracking_id != contact.tracking_id)
            });
            if !desired.contacts.contains_key(&slot) || replace {
                events.push(absolute_event(ABS_MT_SLOT, i32::from(slot)));
                events.push(absolute_event(ABS_MT_TRACKING_ID, -1));
                self.contacts.remove(&slot);
            }
        }

        for (&slot, desired_contact) in &desired.contacts {
            if !self.contacts.contains_key(&slot) {
                let virtual_tracking_id = self.allocate_tracking_id();
                events.push(absolute_event(ABS_MT_SLOT, i32::from(slot)));
                events.push(absolute_event(ABS_MT_TRACKING_ID, virtual_tracking_id));
                let tool_type = desired_contact.tool_type.unwrap_or(0);
                events.push(absolute_event(ABS_MT_TOOL_TYPE, tool_type));
                if let Some(x) = desired_contact.x {
                    events.push(absolute_event(ABS_MT_POSITION_X, x));
                }
                if let Some(y) = desired_contact.y {
                    events.push(absolute_event(ABS_MT_POSITION_Y, y));
                }
                self.contacts.insert(
                    slot,
                    VirtualContact {
                        physical_tracking_id: desired_contact.tracking_id,
                        x: desired_contact.x,
                        y: desired_contact.y,
                        tool_type,
                    },
                );
                continue;
            }

            let current = self.contacts.get_mut(&slot).expect("slot checked above");
            let mut selected = false;
            let desired_tool_type = desired_contact.tool_type.unwrap_or(0);
            if current.tool_type != desired_tool_type {
                events.push(absolute_event(ABS_MT_SLOT, i32::from(slot)));
                selected = true;
                events.push(absolute_event(ABS_MT_TOOL_TYPE, desired_tool_type));
                current.tool_type = desired_tool_type;
            }
            if current.x != desired_contact.x {
                if !selected {
                    events.push(absolute_event(ABS_MT_SLOT, i32::from(slot)));
                    selected = true;
                }
                if let Some(x) = desired_contact.x {
                    events.push(absolute_event(ABS_MT_POSITION_X, x));
                }
                current.x = desired_contact.x;
            }
            if current.y != desired_contact.y {
                if !selected {
                    events.push(absolute_event(ABS_MT_SLOT, i32::from(slot)));
                }
                if let Some(y) = desired_contact.y {
                    events.push(absolute_event(ABS_MT_POSITION_Y, y));
                }
                current.y = desired_contact.y;
            }
        }

        if desired.abs_x != self.abs_x {
            if let Some(x) = desired.abs_x {
                events.push(absolute_event(ABS_X, x));
            }
            self.abs_x = desired.abs_x;
        }
        if desired.abs_y != self.abs_y {
            if let Some(y) = desired.abs_y {
                events.push(absolute_event(ABS_Y, y));
            }
            self.abs_y = desired.abs_y;
        }

        let desired_fingers = desired.contacts.len();
        let desired_touch_down = desired_fingers > 0;
        if self.button_down != desired.button_down {
            events.push(key_event(BTN_LEFT, if desired.button_down { 1 } else { 0 }));
            self.button_down = desired.button_down;
        }
        if self.touch_down != desired_touch_down {
            events.push(key_event(BTN_TOUCH, if desired_touch_down { 1 } else { 0 }));
            self.touch_down = desired_touch_down;
        }
        if self.tool_fingers != desired_fingers {
            if let Some(code) = tool_key(self.tool_fingers) {
                events.push(key_event(code, 0));
            }
            if let Some(code) = tool_key(desired_fingers) {
                events.push(key_event(code, 1));
            }
            self.tool_fingers = desired_fingers;
        }

        events
    }

    fn pending_snapshot(&self, physical: &PhysicalSnapshot) -> PhysicalSnapshot {
        let mut snapshot = PhysicalSnapshot {
            abs_x: self.abs_x,
            abs_y: self.abs_y,
            button_down: physical.button_down,
            ..PhysicalSnapshot::default()
        };

        for (&slot, current) in &self.contacts {
            if let Some(contact) = physical.contacts.get(&slot) {
                if contact.tracking_id == current.physical_tracking_id {
                    snapshot.contacts.insert(slot, contact.clone());
                }
            }
        }
        if snapshot.contacts.is_empty() {
            snapshot.abs_x = None;
            snapshot.abs_y = None;
            snapshot.button_down = false;
        }
        snapshot
    }

    fn allocate_tracking_id(&mut self) -> i32 {
        let tracking_id = self.next_tracking_id;
        self.next_tracking_id = self.next_tracking_id.checked_add(1).unwrap_or(1);
        tracking_id
    }
}

#[derive(Default)]
struct TouchpadRouter {
    mode: RoutingMode,
    physical: PhysicalState,
    virtual_state: VirtualSynthesizer,
}

impl TouchpadRouter {
    fn push(&mut self, event: &RawInputEvent) -> Option<RoutedPacket> {
        self.physical.apply(event);
        if event.event_type == EVENT_SYNCHRONIZATION && event.code == SYN_REPORT {
            return Some(
                self.finish_frame(
                    event
                        .timestamp_micros
                        .or(self.physical.latest_timestamp_micros)
                        .unwrap_or_default(),
                ),
            );
        }
        None
    }

    fn finish_frame(&mut self, timestamp_micros: u128) -> RoutedPacket {
        let physical = self.physical.snapshot();
        let fingers = physical.fingers();

        let (outcome, desired) = match self.mode {
            RoutingMode::Forwarding if fingers >= 3 => {
                self.mode = RoutingMode::Suppressing;
                (
                    RouteOutcome::ReleasedAndSuppressed,
                    PhysicalSnapshot::empty(),
                )
            }
            RoutingMode::Forwarding if fingers == 2 => {
                self.mode = RoutingMode::PendingTwo {
                    started_micros: timestamp_micros,
                };
                (
                    RouteOutcome::PendingTwo,
                    self.virtual_state.pending_snapshot(&physical),
                )
            }
            RoutingMode::Forwarding => (RouteOutcome::Forwarded, physical),
            RoutingMode::PendingTwo { .. } if fingers >= 3 => {
                self.mode = RoutingMode::Suppressing;
                (
                    RouteOutcome::ReleasedAndSuppressed,
                    PhysicalSnapshot::empty(),
                )
            }
            RoutingMode::PendingTwo { .. } if fingers < 2 => {
                self.mode = RoutingMode::Forwarding;
                (RouteOutcome::Forwarded, physical)
            }
            RoutingMode::PendingTwo { started_micros }
                if timestamp_micros.saturating_sub(started_micros)
                    >= TWO_FINGER_ARBITRATION_MICROS =>
            {
                self.mode = RoutingMode::Forwarding;
                (RouteOutcome::Forwarded, physical)
            }
            RoutingMode::PendingTwo { .. } => (
                RouteOutcome::PendingTwo,
                self.virtual_state.pending_snapshot(&physical),
            ),
            RoutingMode::Suppressing if fingers == 0 => {
                self.mode = RoutingMode::Forwarding;
                (RouteOutcome::Suppressed, PhysicalSnapshot::empty())
            }
            RoutingMode::Suppressing => (RouteOutcome::Suppressed, PhysicalSnapshot::empty()),
        };

        RoutedPacket {
            outcome,
            events: self.virtual_state.synthesize(&desired),
        }
    }

    fn release_packet(&mut self) -> Vec<EvdevInputEvent> {
        self.virtual_state.synthesize(&PhysicalSnapshot::empty())
    }
}

#[derive(Default)]
struct OutputValidator {
    current_slot: u16,
    active_tracking_ids: BTreeMap<u16, i32>,
}

impl OutputValidator {
    fn validate(&mut self, events: &[EvdevInputEvent]) -> Result<()> {
        let mut current_slot = self.current_slot;
        let mut active_tracking_ids = self.active_tracking_ids.clone();
        let mut slot_selected = false;

        for event in events {
            if event.event_type() != EventType::ABSOLUTE {
                continue;
            }
            match event.code() {
                ABS_MT_SLOT if event.value() >= 0 => {
                    current_slot = event.value() as u16;
                    slot_selected = true;
                }
                ABS_MT_SLOT => bail!("virtual touchpad selected a negative slot"),
                ABS_MT_TRACKING_ID if !slot_selected => {
                    bail!("virtual tracking event did not explicitly select a slot in this frame")
                }
                ABS_MT_TRACKING_ID if event.value() < 0 => {
                    if active_tracking_ids.remove(&current_slot).is_none() {
                        bail!(
                            "virtual slot {} was released while already inactive",
                            current_slot
                        );
                    }
                }
                ABS_MT_TRACKING_ID => {
                    if active_tracking_ids.iter().any(|(&slot, &tracking_id)| {
                        slot != current_slot && tracking_id == event.value()
                    }) {
                        bail!(
                            "virtual tracking ID {} was already active in another slot",
                            event.value()
                        );
                    }
                    if let Some(previous) = active_tracking_ids.insert(current_slot, event.value())
                    {
                        bail!(
                            "virtual slot {} received tracking ID {} while tracking ID {} was still active",
                            current_slot,
                            event.value(),
                            previous
                        );
                    }
                }
                ABS_MT_POSITION_X | ABS_MT_POSITION_Y | ABS_MT_TOOL_TYPE if !slot_selected => {
                    bail!("virtual contact update did not explicitly select a slot in this frame")
                }
                _ => {}
            }
        }

        self.current_slot = current_slot;
        self.active_tracking_ids = active_tracking_ids;
        Ok(())
    }
}

fn tool_key(fingers: usize) -> Option<u16> {
    match fingers {
        1 => Some(BTN_TOOL_FINGER),
        2 => Some(BTN_TOOL_DOUBLETAP),
        3 => Some(BTN_TOOL_TRIPLETAP),
        4 => Some(BTN_TOOL_QUADTAP),
        5 => Some(BTN_TOOL_QUINTTAP),
        _ => None,
    }
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

    fn event(timestamp_micros: u128, event_type: &str, code: u16, value: i32) -> RawInputEvent {
        RawInputEvent {
            timestamp_micros: Some(timestamp_micros),
            event_type: event_type.to_owned(),
            code,
            value,
            summary: String::new(),
        }
    }

    fn absolute(timestamp_micros: u128, code: u16, value: i32) -> RawInputEvent {
        event(timestamp_micros, EVENT_ABSOLUTE, code, value)
    }

    fn key(timestamp_micros: u128, code: u16, value: i32) -> RawInputEvent {
        event(timestamp_micros, EVENT_KEY, code, value)
    }

    fn sync(timestamp_micros: u128) -> RawInputEvent {
        event(timestamp_micros, EVENT_SYNCHRONIZATION, SYN_REPORT, 0)
    }

    fn begin_contact(
        router: &mut TouchpadRouter,
        timestamp_micros: u128,
        slot: Option<u16>,
        tracking_id: i32,
        x: i32,
        y: i32,
    ) {
        if let Some(slot) = slot {
            assert!(router
                .push(&absolute(timestamp_micros, ABS_MT_SLOT, i32::from(slot)))
                .is_none());
        }
        assert!(router
            .push(&absolute(timestamp_micros, ABS_MT_TRACKING_ID, tracking_id))
            .is_none());
        assert!(router
            .push(&absolute(timestamp_micros, ABS_MT_POSITION_X, x))
            .is_none());
        assert!(router
            .push(&absolute(timestamp_micros, ABS_MT_POSITION_Y, y))
            .is_none());
    }

    fn finish(router: &mut TouchpadRouter, timestamp_micros: u128) -> RoutedPacket {
        router.push(&sync(timestamp_micros)).unwrap()
    }

    fn validate_all(validator: &mut OutputValidator, packet: &RoutedPacket) {
        validator.validate(&packet.events).unwrap();
    }

    fn tracking_events(packet: &RoutedPacket) -> Vec<(u16, i32)> {
        let mut slot = 0;
        let mut output = Vec::new();
        for event in &packet.events {
            if event.event_type() != EventType::ABSOLUTE {
                continue;
            }
            if event.code() == ABS_MT_SLOT {
                slot = event.value() as u16;
            } else if event.code() == ABS_MT_TRACKING_ID {
                output.push((slot, event.value()));
            }
        }
        output
    }

    #[test]
    fn one_finger_is_synthesized_with_explicit_slot_and_virtual_tracking_id() {
        let mut router = TouchpadRouter::default();
        begin_contact(&mut router, 1_000, Some(0), 329, 100, 200);
        router.push(&key(1_000, BTN_TOOL_FINGER, 1));
        let packet = finish(&mut router, 1_000);

        assert_eq!(packet.outcome, RouteOutcome::Forwarded);
        assert_eq!(tracking_events(&packet), vec![(0, 1)]);
        assert_ne!(tracking_events(&packet)[0].1, 329);
    }

    #[test]
    fn second_finger_waits_and_third_finger_suppresses_without_exposing_two_contacts() {
        let mut router = TouchpadRouter::default();
        let mut validator = OutputValidator::default();

        begin_contact(&mut router, 1_000, Some(0), 10, 100, 200);
        router.push(&key(1_000, BTN_TOOL_FINGER, 1));
        validate_all(&mut validator, &finish(&mut router, 1_000));

        router.push(&key(20_000, BTN_TOOL_FINGER, 0));
        begin_contact(&mut router, 20_000, Some(1), 11, 300, 400);
        router.push(&key(20_000, BTN_TOOL_DOUBLETAP, 1));
        let pending = finish(&mut router, 20_000);
        assert_eq!(pending.outcome, RouteOutcome::PendingTwo);
        assert!(tracking_events(&pending).is_empty());
        validate_all(&mut validator, &pending);

        router.push(&key(60_000, BTN_TOOL_DOUBLETAP, 0));
        begin_contact(&mut router, 60_000, Some(2), 12, 500, 600);
        router.push(&key(60_000, BTN_TOOL_TRIPLETAP, 1));
        let suppressed = finish(&mut router, 60_000);
        assert_eq!(suppressed.outcome, RouteOutcome::ReleasedAndSuppressed);
        assert_eq!(tracking_events(&suppressed), vec![(0, -1)]);
        validate_all(&mut validator, &suppressed);
        assert!(validator.active_tracking_ids.is_empty());
    }

    #[test]
    fn two_fingers_commit_after_arbitration_window() {
        let mut router = TouchpadRouter::default();
        let mut validator = OutputValidator::default();

        begin_contact(&mut router, 1_000, Some(0), 10, 100, 200);
        router.push(&key(1_000, BTN_TOOL_FINGER, 1));
        validate_all(&mut validator, &finish(&mut router, 1_000));

        router.push(&key(10_000, BTN_TOOL_FINGER, 0));
        begin_contact(&mut router, 10_000, Some(1), 11, 300, 400);
        router.push(&key(10_000, BTN_TOOL_DOUBLETAP, 1));
        let pending = finish(&mut router, 10_000);
        assert_eq!(pending.outcome, RouteOutcome::PendingTwo);
        validate_all(&mut validator, &pending);

        router.push(&absolute(120_000, ABS_MT_SLOT, 1));
        router.push(&absolute(120_000, ABS_MT_POSITION_X, 320));
        let committed = finish(&mut router, 120_000);
        assert_eq!(committed.outcome, RouteOutcome::Forwarded);
        // Slot 0 stayed active throughout the arbitration window, so the
        // commit packet only allocates the newly accepted second contact.
        assert_eq!(tracking_events(&committed).len(), 1);
        assert_eq!(tracking_events(&committed)[0].0, 1);
        validate_all(&mut validator, &committed);
        assert_eq!(validator.active_tracking_ids.len(), 2);
    }

    #[test]
    fn no_slot_prefix_after_synthetic_release_cannot_target_the_wrong_virtual_slot() {
        let mut router = TouchpadRouter::default();
        let mut validator = OutputValidator::default();

        begin_contact(&mut router, 1_000, Some(0), 100, 100, 100);
        router.push(&key(1_000, BTN_TOOL_FINGER, 1));
        validate_all(&mut validator, &finish(&mut router, 1_000));

        router.push(&key(2_000, BTN_TOOL_FINGER, 0));
        begin_contact(&mut router, 2_000, Some(1), 101, 200, 200);
        router.push(&key(2_000, BTN_TOOL_DOUBLETAP, 1));
        validate_all(&mut validator, &finish(&mut router, 2_000));

        router.push(&key(3_000, BTN_TOOL_DOUBLETAP, 0));
        begin_contact(&mut router, 3_000, Some(2), 102, 300, 300);
        router.push(&key(3_000, BTN_TOOL_TRIPLETAP, 1));
        validate_all(&mut validator, &finish(&mut router, 3_000));

        for slot in [2, 1, 0] {
            router.push(&absolute(4_000, ABS_MT_SLOT, slot));
            router.push(&absolute(4_000, ABS_MT_TRACKING_ID, -1));
        }
        router.push(&key(4_000, BTN_TOOL_TRIPLETAP, 0));
        validate_all(&mut validator, &finish(&mut router, 4_000));

        // The physical device retains slot 0 as its current slot and legally
        // starts the next contact without repeating ABS_MT_SLOT. The virtual
        // synthesizer must still emit an explicit slot selector.
        begin_contact(&mut router, 5_000, None, 200, 400, 400);
        router.push(&key(5_000, BTN_TOOL_FINGER, 1));
        let next = finish(&mut router, 5_000);
        assert_eq!(tracking_events(&next).len(), 1);
        assert_eq!(tracking_events(&next)[0].0, 0);
        assert_eq!(
            next.events.first().map(|event| event.code()),
            Some(ABS_MT_SLOT)
        );
        validate_all(&mut validator, &next);
    }

    #[test]
    fn replacing_an_active_physical_tracking_id_releases_before_reallocating() {
        let mut router = TouchpadRouter::default();
        let mut validator = OutputValidator::default();

        begin_contact(&mut router, 1_000, Some(0), 10, 100, 100);
        router.push(&key(1_000, BTN_TOOL_FINGER, 1));
        validate_all(&mut validator, &finish(&mut router, 1_000));

        begin_contact(&mut router, 2_000, Some(0), 20, 200, 200);
        let replacement = finish(&mut router, 2_000);
        let tracking = tracking_events(&replacement);
        assert_eq!(tracking.len(), 2);
        assert_eq!(tracking[0], (0, -1));
        assert_eq!(tracking[1].0, 0);
        assert!(tracking[1].1 > 0);
        validate_all(&mut validator, &replacement);
    }

    #[test]
    fn suppression_continues_until_all_physical_contacts_are_lifted() {
        let mut router = TouchpadRouter::default();

        begin_contact(&mut router, 1_000, Some(0), 10, 100, 100);
        begin_contact(&mut router, 1_000, Some(1), 11, 200, 200);
        begin_contact(&mut router, 1_000, Some(2), 12, 300, 300);
        router.push(&key(1_000, BTN_TOOL_TRIPLETAP, 1));
        assert_eq!(
            finish(&mut router, 1_000).outcome,
            RouteOutcome::ReleasedAndSuppressed
        );

        router.push(&absolute(2_000, ABS_MT_SLOT, 2));
        router.push(&absolute(2_000, ABS_MT_TRACKING_ID, -1));
        router.push(&key(2_000, BTN_TOOL_TRIPLETAP, 0));
        router.push(&key(2_000, BTN_TOOL_DOUBLETAP, 1));
        assert_eq!(finish(&mut router, 2_000).outcome, RouteOutcome::Suppressed);
        assert_eq!(router.mode, RoutingMode::Suppressing);

        for slot in [1, 0] {
            router.push(&absolute(3_000, ABS_MT_SLOT, slot));
            router.push(&absolute(3_000, ABS_MT_TRACKING_ID, -1));
        }
        router.push(&key(3_000, BTN_TOOL_DOUBLETAP, 0));
        assert_eq!(finish(&mut router, 3_000).outcome, RouteOutcome::Suppressed);
        assert_eq!(router.mode, RoutingMode::Forwarding);
    }

    #[test]
    fn output_validator_rejects_double_tracking_ids() {
        let mut validator = OutputValidator::default();
        let invalid = vec![
            absolute_event(ABS_MT_SLOT, 1),
            absolute_event(ABS_MT_TRACKING_ID, 10),
            absolute_event(ABS_MT_SLOT, 1),
            absolute_event(ABS_MT_TRACKING_ID, 11),
        ];
        let error = validator.validate(&invalid).unwrap_err().to_string();
        assert!(error.contains("still active"));

        let duplicate_across_slots = vec![
            absolute_event(ABS_MT_SLOT, 0),
            absolute_event(ABS_MT_TRACKING_ID, 20),
            absolute_event(ABS_MT_SLOT, 1),
            absolute_event(ABS_MT_TRACKING_ID, 20),
        ];
        let error = OutputValidator::default()
            .validate(&duplicate_across_slots)
            .unwrap_err()
            .to_string();
        assert!(error.contains("another slot"));
    }

    #[test]
    fn release_packet_is_idempotent_and_valid() {
        let mut router = TouchpadRouter::default();
        let mut validator = OutputValidator::default();

        begin_contact(&mut router, 1_000, Some(0), 10, 100, 100);
        router.push(&key(1_000, BTN_TOOL_FINGER, 1));
        validate_all(&mut validator, &finish(&mut router, 1_000));

        let first = router.release_packet();
        assert!(!first.is_empty());
        validator.validate(&first).unwrap();
        assert!(router.release_packet().is_empty());
    }

    fn set_reported_fingers(
        router: &mut TouchpadRouter,
        timestamp_micros: u128,
        previous: usize,
        next: usize,
    ) {
        if previous == next {
            return;
        }
        if let Some(code) = tool_key(previous) {
            assert!(router.push(&key(timestamp_micros, code, 0)).is_none());
        }
        if let Some(code) = tool_key(next) {
            assert!(router.push(&key(timestamp_micros, code, 1)).is_none());
        }
    }

    #[test]
    fn captured_no_slot_prefix_sequence_replays_without_double_tracking_ids() {
        let mut router = TouchpadRouter::default();
        let mut validator = OutputValidator::default();

        // Captured physical sequence: one contact begins and ends on slot 0.
        begin_contact(&mut router, 1_000, Some(0), 441, 735, 432);
        router.push(&key(1_000, BTN_TOUCH, 1));
        router.push(&key(1_000, BTN_TOOL_FINGER, 1));
        validate_all(&mut validator, &finish(&mut router, 1_000));

        router.push(&absolute(900_000, ABS_MT_TRACKING_ID, -1));
        router.push(&key(900_000, BTN_TOUCH, 0));
        router.push(&key(900_000, BTN_TOOL_FINGER, 0));
        validate_all(&mut validator, &finish(&mut router, 900_000));

        // The next physical frame legally starts slot 0 without repeating
        // ABS_MT_SLOT, then selects slot 1 for the second contact.
        begin_contact(&mut router, 1_000_000, None, 442, 700, 420);
        begin_contact(&mut router, 1_000_000, Some(1), 443, 760, 440);
        router.push(&key(1_000_000, BTN_TOUCH, 1));
        router.push(&key(1_000_000, BTN_TOOL_DOUBLETAP, 1));
        validate_all(&mut validator, &finish(&mut router, 1_000_000));

        // Commit the pending two-finger session after the arbitration window.
        router.push(&absolute(1_120_000, ABS_MT_SLOT, 1));
        router.push(&absolute(1_120_000, ABS_MT_POSITION_X, 770));
        let committed = finish(&mut router, 1_120_000);
        validate_all(&mut validator, &committed);
        assert_eq!(validator.active_tracking_ids.len(), 2);
        assert_eq!(tracking_events(&committed).len(), 2);
        assert_eq!(tracking_events(&committed)[0].0, 0);
        assert_eq!(tracking_events(&committed)[1].0, 1);

        for slot in [1, 0] {
            router.push(&absolute(1_200_000, ABS_MT_SLOT, slot));
            router.push(&absolute(1_200_000, ABS_MT_TRACKING_ID, -1));
        }
        router.push(&key(1_200_000, BTN_TOUCH, 0));
        router.push(&key(1_200_000, BTN_TOOL_DOUBLETAP, 0));
        validate_all(&mut validator, &finish(&mut router, 1_200_000));
        assert!(validator.active_tracking_ids.is_empty());
    }

    #[test]
    fn deterministic_state_stress_never_emits_an_invalid_mt_stream() {
        let mut router = TouchpadRouter::default();
        let mut validator = OutputValidator::default();
        let mut active = BTreeMap::<u16, i32>::new();
        let mut current_slot = 0_u16;
        let mut next_physical_tracking_id = 1_000_i32;
        let mut reported_fingers = 0_usize;
        let mut seed = 0x6a09_e667_f3bc_c909_u64;

        fn next_random(seed: &mut u64) -> u64 {
            *seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            *seed
        }

        for frame_index in 0..10_000_u128 {
            let timestamp_micros = 10_000 + frame_index * 7_000;
            let random = next_random(&mut seed);
            let operation = (random & 3) as u8;
            let requested_slot = ((random >> 8) % 5) as u16;
            let slot = if (random >> 16) & 1 == 0 {
                current_slot
            } else {
                requested_slot
            };
            let emit_slot = slot != current_slot || ((random >> 17) & 3 == 0);
            if emit_slot {
                assert!(router
                    .push(&absolute(timestamp_micros, ABS_MT_SLOT, i32::from(slot)))
                    .is_none());
                current_slot = slot;
            }

            match operation {
                0 if !active.contains_key(&slot) => {
                    let tracking_id = next_physical_tracking_id;
                    next_physical_tracking_id += 1;
                    active.insert(slot, tracking_id);
                    assert!(router
                        .push(&absolute(timestamp_micros, ABS_MT_TRACKING_ID, tracking_id,))
                        .is_none());
                    assert!(router
                        .push(&absolute(
                            timestamp_micros,
                            ABS_MT_POSITION_X,
                            ((random >> 24) % 1405) as i32,
                        ))
                        .is_none());
                    assert!(router
                        .push(&absolute(
                            timestamp_micros,
                            ABS_MT_POSITION_Y,
                            ((random >> 40) % 865) as i32,
                        ))
                        .is_none());
                }
                1 if active.remove(&slot).is_some() => {
                    assert!(router
                        .push(&absolute(timestamp_micros, ABS_MT_TRACKING_ID, -1))
                        .is_none());
                }
                2 if active.contains_key(&slot) => {
                    assert!(router
                        .push(&absolute(
                            timestamp_micros,
                            ABS_MT_POSITION_X,
                            ((random >> 24) % 1405) as i32,
                        ))
                        .is_none());
                    assert!(router
                        .push(&absolute(
                            timestamp_micros,
                            ABS_MT_POSITION_Y,
                            ((random >> 40) % 865) as i32,
                        ))
                        .is_none());
                }
                3 if active.contains_key(&slot) => {
                    // Deliberately replace an active physical tracking ID.
                    // The virtual synthesizer must release before reallocating.
                    let tracking_id = next_physical_tracking_id;
                    next_physical_tracking_id += 1;
                    active.insert(slot, tracking_id);
                    assert!(router
                        .push(&absolute(timestamp_micros, ABS_MT_TRACKING_ID, tracking_id,))
                        .is_none());
                }
                _ => {}
            }

            let next_reported_fingers = active.len();
            set_reported_fingers(
                &mut router,
                timestamp_micros,
                reported_fingers,
                next_reported_fingers,
            );
            reported_fingers = next_reported_fingers;
            router.push(&key(
                timestamp_micros,
                BTN_TOUCH,
                i32::from(!active.is_empty()),
            ));

            let packet = finish(&mut router, timestamp_micros);
            validate_all(&mut validator, &packet);
        }

        for slot in active.keys().copied().collect::<Vec<_>>() {
            let timestamp_micros = 80_000_000 + u128::from(slot);
            router.push(&absolute(timestamp_micros, ABS_MT_SLOT, i32::from(slot)));
            router.push(&absolute(timestamp_micros, ABS_MT_TRACKING_ID, -1));
        }
        set_reported_fingers(&mut router, 80_100_000, reported_fingers, 0);
        router.push(&key(80_100_000, BTN_TOUCH, 0));
        validate_all(&mut validator, &finish(&mut router, 80_100_000));
        let release = router.release_packet();
        validator.validate(&release).unwrap();
        assert!(validator.active_tracking_ids.is_empty());
    }
}
