use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::RawInputEvent;

const EVENT_SYNCHRONIZATION: &str = "SYNCHRONIZATION";
const EVENT_KEY: &str = "KEY";
const EVENT_ABSOLUTE: &str = "ABSOLUTE";

const SYN_REPORT: u16 = 0;
const ABS_MT_SLOT: u16 = 47;
const ABS_MT_POSITION_X: u16 = 53;
const ABS_MT_POSITION_Y: u16 = 54;
const ABS_MT_TRACKING_ID: u16 = 57;

const BTN_TOOL_FINGER: u16 = 325;
const BTN_TOOL_QUINTTAP: u16 = 328;
const BTN_TOOL_DOUBLETAP: u16 = 333;
const BTN_TOOL_TRIPLETAP: u16 = 334;
const BTN_TOOL_QUADTAP: u16 = 335;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TouchFramePhase {
    Begin,
    Update,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TouchPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TouchContact {
    pub slot: u16,
    pub tracking_id: i32,
    pub x: Option<i32>,
    pub y: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TouchFrame {
    pub sequence: u64,
    pub timestamp_micros: Option<u128>,
    pub phase: TouchFramePhase,
    /// Effective finger count: the greater of tracked and device-reported fingers.
    pub fingers: u8,
    /// Active protocol-B contacts with tracking IDs, whether or not X/Y are known yet.
    pub tracked_contacts: usize,
    pub reported_fingers: Option<u8>,
    /// True when every effective finger has an active contact with complete X/Y.
    #[serde(default)]
    pub tracking_complete: bool,
    pub contacts: Vec<TouchContact>,
    pub centroid: Option<TouchPoint>,
    pub delta: Option<TouchPoint>,
    pub velocity_per_second: Option<TouchPoint>,
    pub frame_interval_micros: Option<u128>,
}

#[derive(Debug, Clone, Default)]
struct SlotState {
    tracking_id: Option<i32>,
    x: Option<i32>,
    y: Option<i32>,
}

/// Converts Linux multitouch protocol-B events into hardware-neutral frames.
///
/// This tracker only understands contact state. It does not recognize swipes,
/// drags, taps, or actions, which keeps later recognizers fully configurable.
#[derive(Debug, Default)]
pub struct TouchFrameTracker {
    current_slot: u16,
    slots: BTreeMap<u16, SlotState>,
    tool_keys: [bool; 6],
    previous_fingers: u8,
    previous_motion_contacts: Vec<(u16, i32)>,
    previous_centroid: Option<TouchPoint>,
    previous_timestamp_micros: Option<u128>,
    sequence: u64,
    dirty: bool,
}

impl TouchFrameTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one raw evdev event. A normalized frame is returned at SYN_REPORT
    /// boundaries whenever active contact state changed.
    pub fn push(&mut self, event: &RawInputEvent) -> Option<TouchFrame> {
        match event.event_type.as_str() {
            EVENT_ABSOLUTE => self.handle_absolute(event.code, event.value),
            EVENT_KEY => self.handle_key(event.code, event.value),
            EVENT_SYNCHRONIZATION if event.code == SYN_REPORT => {
                return self.finish_frame(event.timestamp_micros);
            }
            _ => {}
        }
        None
    }

    pub fn active_contacts(&self) -> Vec<TouchContact> {
        self.slots
            .iter()
            .filter_map(|(&slot, state)| {
                state.tracking_id.map(|tracking_id| TouchContact {
                    slot,
                    tracking_id,
                    x: state.x,
                    y: state.y,
                })
            })
            .collect()
    }

    fn handle_absolute(&mut self, code: u16, value: i32) {
        match code {
            ABS_MT_SLOT if value >= 0 => {
                self.current_slot = value as u16;
            }
            ABS_MT_TRACKING_ID => {
                if value < 0 {
                    self.slots.remove(&self.current_slot);
                } else {
                    self.slots.insert(
                        self.current_slot,
                        SlotState {
                            tracking_id: Some(value),
                            x: None,
                            y: None,
                        },
                    );
                }
                self.dirty = true;
            }
            ABS_MT_POSITION_X => {
                self.slots.entry(self.current_slot).or_default().x = Some(value);
                self.dirty = true;
            }
            ABS_MT_POSITION_Y => {
                self.slots.entry(self.current_slot).or_default().y = Some(value);
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, code: u16, value: i32) {
        let fingers = match code {
            BTN_TOOL_FINGER => Some(1),
            BTN_TOOL_DOUBLETAP => Some(2),
            BTN_TOOL_TRIPLETAP => Some(3),
            BTN_TOOL_QUADTAP => Some(4),
            BTN_TOOL_QUINTTAP => Some(5),
            _ => None,
        };

        if let Some(fingers) = fingers {
            self.tool_keys[fingers] = value != 0;
            self.dirty = true;
        }
    }

    fn finish_frame(&mut self, timestamp_micros: Option<u128>) -> Option<TouchFrame> {
        let contacts = self.active_contacts();
        let tracked_contacts = contacts.len();
        let reported_fingers = self.reported_fingers();
        let fingers = usize::from(reported_fingers.unwrap_or_default())
            .max(tracked_contacts)
            .min(usize::from(u8::MAX)) as u8;
        let motion_contacts = coordinate_contact_ids(&contacts);
        let tracking_complete =
            motion_contacts.len() == tracked_contacts && tracked_contacts == usize::from(fingers);

        if !self.dirty {
            return None;
        }

        let phase = if self.previous_fingers == 0 && fingers > 0 {
            TouchFramePhase::Begin
        } else if self.previous_fingers > 0 && fingers == 0 {
            TouchFramePhase::End
        } else {
            TouchFramePhase::Update
        };

        let centroid = centroid(&contacts);
        let same_motion_basis = fingers > 0
            && fingers == self.previous_fingers
            && motion_contacts == self.previous_motion_contacts;
        let frame_interval_micros = if same_motion_basis {
            elapsed_micros(self.previous_timestamp_micros, timestamp_micros)
        } else {
            None
        };
        let delta = if same_motion_basis {
            point_delta(self.previous_centroid, centroid)
        } else {
            None
        };
        let velocity_per_second = match (delta, frame_interval_micros) {
            (Some(delta), Some(elapsed)) if elapsed > 0 => {
                let scale = 1_000_000.0 / elapsed as f64;
                Some(TouchPoint {
                    x: delta.x * scale,
                    y: delta.y * scale,
                })
            }
            _ => None,
        };

        self.sequence += 1;
        let frame = TouchFrame {
            sequence: self.sequence,
            timestamp_micros,
            phase,
            fingers,
            tracked_contacts,
            reported_fingers,
            tracking_complete,
            contacts,
            centroid,
            delta,
            velocity_per_second,
            frame_interval_micros,
        };

        self.previous_fingers = fingers;
        self.previous_motion_contacts = motion_contacts;
        self.previous_timestamp_micros = timestamp_micros;
        self.previous_centroid = if fingers == 0 { None } else { centroid };
        self.dirty = false;

        Some(frame)
    }

    fn reported_fingers(&self) -> Option<u8> {
        self.tool_keys
            .iter()
            .rposition(|active| *active)
            .and_then(|index| u8::try_from(index).ok())
            .filter(|fingers| *fingers > 0)
    }
}

fn centroid(contacts: &[TouchContact]) -> Option<TouchPoint> {
    let points: Vec<_> = contacts
        .iter()
        .filter_map(|contact| Some((f64::from(contact.x?), f64::from(contact.y?))))
        .collect();

    if points.is_empty() {
        return None;
    }

    let count = points.len() as f64;
    Some(TouchPoint {
        x: points.iter().map(|point| point.0).sum::<f64>() / count,
        y: points.iter().map(|point| point.1).sum::<f64>() / count,
    })
}

fn coordinate_contact_ids(contacts: &[TouchContact]) -> Vec<(u16, i32)> {
    contacts
        .iter()
        .filter(|contact| contact.x.is_some() && contact.y.is_some())
        .map(|contact| (contact.slot, contact.tracking_id))
        .collect()
}

fn point_delta(previous: Option<TouchPoint>, current: Option<TouchPoint>) -> Option<TouchPoint> {
    let (previous, current) = (previous?, current?);
    Some(TouchPoint {
        x: current.x - previous.x,
        y: current.y - previous.y,
    })
}

fn elapsed_micros(previous: Option<u128>, current: Option<u128>) -> Option<u128> {
    current?.checked_sub(previous?)
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
        tracker: &mut TouchFrameTracker,
        timestamp_micros: u128,
        slot: i32,
        tracking_id: i32,
        x: i32,
        y: i32,
    ) {
        tracker.push(&absolute(timestamp_micros, ABS_MT_SLOT, slot));
        tracker.push(&absolute(timestamp_micros, ABS_MT_TRACKING_ID, tracking_id));
        tracker.push(&absolute(timestamp_micros, ABS_MT_POSITION_X, x));
        tracker.push(&absolute(timestamp_micros, ABS_MT_POSITION_Y, y));
    }

    #[test]
    fn builds_begin_update_and_end_frames() {
        let mut tracker = TouchFrameTracker::new();
        begin_contact(&mut tracker, 1_000_000, 0, 10, 100, 200);
        tracker.push(&key(1_000_000, BTN_TOOL_FINGER, 1));

        let begin = tracker.push(&sync(1_000_000)).unwrap();
        assert_eq!(begin.phase, TouchFramePhase::Begin);
        assert_eq!(begin.fingers, 1);
        assert_eq!(begin.centroid, Some(TouchPoint { x: 100.0, y: 200.0 }));
        assert_eq!(begin.delta, None);

        tracker.push(&absolute(1_010_000, ABS_MT_POSITION_X, 110));
        tracker.push(&absolute(1_010_000, ABS_MT_POSITION_Y, 220));
        let update = tracker.push(&sync(1_010_000)).unwrap();
        assert_eq!(update.phase, TouchFramePhase::Update);
        assert_eq!(update.delta, Some(TouchPoint { x: 10.0, y: 20.0 }));
        assert_eq!(update.frame_interval_micros, Some(10_000));
        assert_eq!(
            update.velocity_per_second,
            Some(TouchPoint {
                x: 1_000.0,
                y: 2_000.0,
            })
        );

        tracker.push(&absolute(1_020_000, ABS_MT_TRACKING_ID, -1));
        tracker.push(&key(1_020_000, BTN_TOOL_FINGER, 0));
        let end = tracker.push(&sync(1_020_000)).unwrap();
        assert_eq!(end.phase, TouchFramePhase::End);
        assert_eq!(end.fingers, 0);
        assert!(end.contacts.is_empty());
    }

    #[test]
    fn tracks_multiple_protocol_b_slots() {
        let mut tracker = TouchFrameTracker::new();
        begin_contact(&mut tracker, 1_000, 0, 20, 100, 100);
        begin_contact(&mut tracker, 1_000, 1, 21, 200, 200);
        begin_contact(&mut tracker, 1_000, 2, 22, 300, 300);
        tracker.push(&key(1_000, BTN_TOOL_TRIPLETAP, 1));

        let frame = tracker.push(&sync(1_000)).unwrap();
        assert_eq!(frame.fingers, 3);
        assert_eq!(frame.tracked_contacts, 3);
        assert_eq!(frame.reported_fingers, Some(3));
        assert!(frame.tracking_complete);
        assert_eq!(frame.contacts[0].slot, 0);
        assert_eq!(frame.contacts[2].slot, 2);
        assert_eq!(frame.centroid, Some(TouchPoint { x: 200.0, y: 200.0 }));
    }

    #[test]
    fn tracks_four_independent_slots() {
        let mut tracker = TouchFrameTracker::new();
        begin_contact(&mut tracker, 1_000, 0, 50, 100, 100);
        begin_contact(&mut tracker, 1_000, 1, 51, 200, 100);
        begin_contact(&mut tracker, 1_000, 2, 52, 100, 200);
        begin_contact(&mut tracker, 1_000, 3, 53, 200, 200);
        tracker.push(&key(1_000, BTN_TOOL_QUADTAP, 1));

        let frame = tracker.push(&sync(1_000)).unwrap();
        assert_eq!(frame.fingers, 4);
        assert_eq!(frame.tracked_contacts, 4);
        assert!(frame.tracking_complete);
        assert_eq!(frame.contacts[3].slot, 3);
        assert_eq!(frame.centroid, Some(TouchPoint { x: 150.0, y: 150.0 }));
    }

    #[test]
    fn preserves_tool_count_when_hardware_tracks_fewer_contacts() {
        let mut tracker = TouchFrameTracker::new();
        begin_contact(&mut tracker, 1_000, 0, 30, 100, 100);
        begin_contact(&mut tracker, 1_000, 1, 31, 200, 200);
        tracker.push(&key(1_000, BTN_TOOL_QUADTAP, 1));

        let frame = tracker.push(&sync(1_000)).unwrap();
        assert_eq!(frame.fingers, 4);
        assert_eq!(frame.tracked_contacts, 2);
        assert_eq!(frame.reported_fingers, Some(4));
        assert!(!frame.tracking_complete);
    }

    #[test]
    fn incomplete_coordinates_are_not_complete_tracking() {
        let mut tracker = TouchFrameTracker::new();
        tracker.push(&absolute(1_000, ABS_MT_SLOT, 0));
        tracker.push(&absolute(1_000, ABS_MT_TRACKING_ID, 60));
        tracker.push(&absolute(1_000, ABS_MT_POSITION_X, 100));
        tracker.push(&key(1_000, BTN_TOOL_FINGER, 1));

        let frame = tracker.push(&sync(1_000)).unwrap();
        assert_eq!(frame.tracked_contacts, 1);
        assert!(!frame.tracking_complete);
        assert_eq!(frame.centroid, None);
    }

    #[test]
    fn resets_motion_when_coordinate_contact_membership_changes() {
        let mut tracker = TouchFrameTracker::new();
        begin_contact(&mut tracker, 1_000, 0, 70, 100, 100);
        tracker.push(&key(1_000, BTN_TOOL_DOUBLETAP, 1));
        tracker.push(&sync(1_000));

        tracker.push(&absolute(2_000, ABS_MT_SLOT, 1));
        tracker.push(&absolute(2_000, ABS_MT_TRACKING_ID, 71));
        tracker.push(&absolute(2_000, ABS_MT_POSITION_X, 400));
        tracker.push(&absolute(2_000, ABS_MT_POSITION_Y, 400));
        let frame = tracker.push(&sync(2_000)).unwrap();

        assert_eq!(frame.fingers, 2);
        assert!(frame.tracking_complete);
        assert_eq!(frame.delta, None);
        assert_eq!(frame.velocity_per_second, None);
    }

    #[test]
    fn resets_motion_when_finger_count_changes() {
        let mut tracker = TouchFrameTracker::new();
        begin_contact(&mut tracker, 1_000, 0, 40, 100, 100);
        tracker.push(&key(1_000, BTN_TOOL_FINGER, 1));
        tracker.push(&sync(1_000));

        begin_contact(&mut tracker, 2_000, 1, 41, 400, 400);
        tracker.push(&key(2_000, BTN_TOOL_FINGER, 0));
        tracker.push(&key(2_000, BTN_TOOL_DOUBLETAP, 1));
        let frame = tracker.push(&sync(2_000)).unwrap();

        assert_eq!(frame.fingers, 2);
        assert_eq!(frame.delta, None);
        assert_eq!(frame.velocity_per_second, None);
    }

    #[test]
    fn ignores_timestamp_only_reports() {
        let mut tracker = TouchFrameTracker::new();
        assert_eq!(tracker.push(&sync(1_000)), None);
    }
}
