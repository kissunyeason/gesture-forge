use std::collections::BTreeMap;

use anyhow::Result;
use gesture_core::InputEvent;
use gesture_device::{TouchFrame, TouchFramePhase, TouchPoint};
use serde::{Deserialize, Serialize};

use crate::{DragRuleConfig, RecognizerConfig};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GestureSessionMetrics {
    pub fingers: u8,
    pub tracking_complete: bool,
    pub points: usize,
    pub duration_ms: f64,
    pub dx: f64,
    pub dy: f64,
    pub distance: f64,
    pub path_length: f64,
    pub average_velocity: f64,
    pub axis_deviation_degrees: f64,
    pub straightness: f64,
    pub direction: String,
}

#[derive(Debug)]
pub struct GestureRecognizer {
    config: RecognizerConfig,
    session: Vec<TouchFrame>,
    drag_candidate: Option<DragCandidate>,
    active_drag: Option<ActiveDrag>,
    session_claimed: bool,
}

impl GestureRecognizer {
    pub fn new(config: RecognizerConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            session: Vec::new(),
            drag_candidate: None,
            active_drag: None,
            session_claimed: false,
        })
    }

    pub fn config(&self) -> &RecognizerConfig {
        &self.config
    }

    /// Consume one normalized touch frame and return zero or more action-agnostic
    /// gesture events. Swipes and holds are classified at session end; drag
    /// rules publish a continuous begin/update/end/cancel lifecycle.
    pub fn push(&mut self, frame: &TouchFrame) -> Vec<InputEvent> {
        if frame.phase == TouchFramePhase::Begin {
            self.session.clear();
            self.drag_candidate = None;
            self.active_drag = None;
            self.session_claimed = false;
        }

        if self.session.is_empty() && frame.fingers == 0 {
            return Vec::new();
        }

        self.session.push(frame.clone());
        let drag_events = self.update_drag(frame);

        if frame.phase != TouchFramePhase::End {
            return drag_events;
        }

        let events = if self.session_claimed {
            drag_events
        } else {
            self.classify_completed_session()
        };
        self.session.clear();
        self.drag_candidate = None;
        self.active_drag = None;
        self.session_claimed = false;
        events
    }

    fn update_drag(&mut self, frame: &TouchFrame) -> Vec<InputEvent> {
        if let Some(mut active) = self.active_drag.take() {
            if frame.phase == TouchFramePhase::End {
                self.session_claimed = true;
                return vec![active.finish_event("end", frame.timestamp_micros)];
            }

            let valid_tracking = !active.require_complete_tracking || frame.tracking_complete;
            let sample = match (
                frame.fingers == active.fingers,
                valid_tracking,
                frame.timestamp_micros,
                frame.centroid,
            ) {
                (true, true, Some(timestamp), Some(point)) => Some((timestamp, point)),
                _ => None,
            };

            let Some((timestamp, point)) = sample else {
                self.session_claimed = true;
                return vec![active.finish_event("cancel", frame.timestamp_micros)];
            };
            if coordinate_contact_ids(frame) != active.contact_ids {
                self.session_claimed = true;
                return vec![active.finish_event("cancel", Some(timestamp))];
            }

            let event = active.update_event(timestamp, point, frame.tracking_complete);
            self.active_drag = Some(active);
            return vec![event];
        }

        if self.session_claimed || frame.phase == TouchFramePhase::End {
            self.drag_candidate = None;
            return Vec::new();
        }

        let Some((rule_index, rule)) = self
            .config
            .recognition
            .drags
            .iter()
            .enumerate()
            .find(|(_, rule)| {
                rule.enabled
                    && frame.fingers == rule.fingers
                    && (!rule.require_complete_tracking || frame.tracking_complete)
            })
            .map(|(index, rule)| (index, rule.clone()))
        else {
            self.drag_candidate = None;
            return Vec::new();
        };
        let (Some(timestamp), Some(point)) = (frame.timestamp_micros, frame.centroid) else {
            self.drag_candidate = None;
            return Vec::new();
        };
        let contact_ids = coordinate_contact_ids(frame);

        if !self.drag_candidate.as_ref().is_some_and(|candidate| {
            candidate.rule_index == rule_index && candidate.contact_ids == contact_ids
        }) {
            self.drag_candidate = Some(DragCandidate::new(
                rule_index,
                timestamp,
                point,
                contact_ids,
                frame.tracking_complete,
            ));
            return Vec::new();
        }

        let candidate = self
            .drag_candidate
            .as_mut()
            .expect("candidate checked above");
        if candidate.armed_point.is_none() {
            if point_distance(candidate.hold_start_point, point) > rule.max_hold_distance {
                *candidate = DragCandidate::new(
                    rule_index,
                    timestamp,
                    point,
                    candidate.contact_ids.clone(),
                    frame.tracking_complete,
                );
                return Vec::new();
            }

            let hold_duration_ms =
                elapsed_millis(candidate.hold_start_timestamp, timestamp).unwrap_or_default();
            if hold_duration_ms >= rule.min_hold_duration_ms {
                candidate.armed_point = Some(point);
                candidate.armed_timestamp = Some(timestamp);
                candidate.hold_duration_ms = hold_duration_ms;
            }
        }
        candidate.tracking_complete &= frame.tracking_complete;

        let Some(armed_point) = candidate.armed_point else {
            return Vec::new();
        };
        if point_distance(armed_point, point) < rule.min_drag_distance {
            return Vec::new();
        }

        let active = ActiveDrag::new(
            &rule,
            armed_point,
            point,
            candidate
                .armed_timestamp
                .expect("armed timestamp accompanies armed point"),
            timestamp,
            candidate.hold_duration_ms,
            candidate.contact_ids.clone(),
            candidate.tracking_complete,
        );
        let event = active.event(
            "begin",
            TouchPoint {
                x: point.x - armed_point.x,
                y: point.y - armed_point.y,
            },
            timestamp,
        );
        self.drag_candidate = None;
        self.active_drag = Some(active);
        self.session_claimed = true;
        vec![event]
    }

    fn classify_completed_session(&self) -> Vec<InputEvent> {
        let swipe = self
            .config
            .recognition
            .swipes
            .iter()
            .enumerate()
            .filter(|(_, rule)| rule.enabled)
            .filter_map(|(order, rule)| {
                best_segment_metrics(&self.session, rule.fingers, rule.require_complete_tracking)
                    .filter(|metrics| {
                        metrics.distance >= rule.min_distance
                            && metrics.average_velocity >= rule.min_average_velocity
                            && metrics.duration_ms <= rule.max_duration_ms
                            && metrics.axis_deviation_degrees <= rule.max_axis_deviation_degrees
                    })
                    .map(|metrics| RuleCandidate {
                        rule_id: &rule.id,
                        order,
                        metrics,
                    })
            })
            .max_by(compare_candidates);
        if let Some(candidate) = swipe {
            return vec![swipe_event(candidate.metrics, candidate.rule_id)];
        }

        let hold = self
            .config
            .recognition
            .holds
            .iter()
            .enumerate()
            .filter(|(_, rule)| rule.enabled)
            .filter_map(|(order, rule)| {
                best_segment_metrics(&self.session, rule.fingers, rule.require_complete_tracking)
                    .filter(|metrics| {
                        metrics.duration_ms >= rule.min_duration_ms
                            && metrics.distance <= rule.max_net_distance
                    })
                    .map(|metrics| RuleCandidate {
                        rule_id: &rule.id,
                        order,
                        metrics,
                    })
            })
            .max_by(compare_candidates);
        if let Some(candidate) = hold {
            return vec![hold_event(candidate.metrics, candidate.rule_id)];
        }

        Vec::new()
    }
}

#[derive(Debug)]
struct DragCandidate {
    rule_index: usize,
    hold_start_timestamp: u128,
    hold_start_point: TouchPoint,
    armed_timestamp: Option<u128>,
    armed_point: Option<TouchPoint>,
    hold_duration_ms: f64,
    contact_ids: Vec<(u16, i32)>,
    tracking_complete: bool,
}

impl DragCandidate {
    fn new(
        rule_index: usize,
        timestamp: u128,
        point: TouchPoint,
        contact_ids: Vec<(u16, i32)>,
        tracking_complete: bool,
    ) -> Self {
        Self {
            rule_index,
            hold_start_timestamp: timestamp,
            hold_start_point: point,
            armed_timestamp: None,
            armed_point: None,
            hold_duration_ms: 0.0,
            contact_ids,
            tracking_complete,
        }
    }
}

#[derive(Debug)]
struct ActiveDrag {
    rule_id: String,
    fingers: u8,
    require_complete_tracking: bool,
    origin: TouchPoint,
    last_point: TouchPoint,
    activated_timestamp: u128,
    last_timestamp: u128,
    hold_duration_ms: f64,
    path_length: f64,
    tracking_complete: bool,
    contact_ids: Vec<(u16, i32)>,
}

impl ActiveDrag {
    #[allow(clippy::too_many_arguments)]
    fn new(
        rule: &DragRuleConfig,
        origin: TouchPoint,
        point: TouchPoint,
        activated_timestamp: u128,
        timestamp: u128,
        hold_duration_ms: f64,
        contact_ids: Vec<(u16, i32)>,
        tracking_complete: bool,
    ) -> Self {
        Self {
            rule_id: rule.id.clone(),
            fingers: rule.fingers,
            require_complete_tracking: rule.require_complete_tracking,
            origin,
            last_point: point,
            activated_timestamp,
            last_timestamp: timestamp,
            hold_duration_ms,
            path_length: point_distance(origin, point),
            tracking_complete,
            contact_ids,
        }
    }

    fn update_event(
        &mut self,
        timestamp: u128,
        point: TouchPoint,
        tracking_complete: bool,
    ) -> InputEvent {
        let delta = TouchPoint {
            x: point.x - self.last_point.x,
            y: point.y - self.last_point.y,
        };
        self.path_length += point_distance(self.last_point, point);
        self.last_point = point;
        self.last_timestamp = timestamp;
        self.tracking_complete &= tracking_complete;
        self.event("update", delta, timestamp)
    }

    fn finish_event(&mut self, phase: &str, timestamp: Option<u128>) -> InputEvent {
        self.event(
            phase,
            TouchPoint { x: 0.0, y: 0.0 },
            timestamp.unwrap_or(self.last_timestamp),
        )
    }

    fn event(&self, phase: &str, delta: TouchPoint, timestamp: u128) -> InputEvent {
        let total_dx = self.last_point.x - self.origin.x;
        let total_dy = self.last_point.y - self.origin.y;
        let mut event = InputEvent::new("touchpad.drag", phase);
        event.fingers = Some(self.fingers);
        event.values.insert("dx".to_owned(), delta.x);
        event.values.insert("dy".to_owned(), delta.y);
        event.values.insert("total_dx".to_owned(), total_dx);
        event.values.insert("total_dy".to_owned(), total_dy);
        event
            .values
            .insert("distance".to_owned(), total_dx.hypot(total_dy));
        event
            .values
            .insert("path_length".to_owned(), self.path_length);
        event.values.insert(
            "duration_ms".to_owned(),
            elapsed_millis(self.activated_timestamp, timestamp).unwrap_or_default(),
        );
        event
            .values
            .insert("hold_duration_ms".to_owned(), self.hold_duration_ms);
        event
            .labels
            .insert("recognizer".to_owned(), "continuous-v1".to_owned());
        event
            .labels
            .insert("recognize_on".to_owned(), "live-frame".to_owned());
        event
            .labels
            .insert("recognition.rule_id".to_owned(), self.rule_id.clone());
        event.labels.insert(
            "tracking_complete".to_owned(),
            self.tracking_complete.to_string(),
        );
        event
    }
}

struct RuleCandidate<'a> {
    rule_id: &'a str,
    order: usize,
    metrics: GestureSessionMetrics,
}

fn compare_candidates(left: &RuleCandidate<'_>, right: &RuleCandidate<'_>) -> std::cmp::Ordering {
    left.metrics
        .duration_ms
        .total_cmp(&right.metrics.duration_ms)
        .then_with(|| left.metrics.points.cmp(&right.metrics.points))
        // Earlier declaration wins an otherwise exact tie.
        .then_with(|| right.order.cmp(&left.order))
}

fn swipe_event(metrics: GestureSessionMetrics, rule_id: &str) -> InputEvent {
    let mut event = InputEvent::new("touchpad.swipe", "end");
    event.fingers = Some(metrics.fingers);
    event.direction = Some(metrics.direction.clone());
    add_metrics(&mut event.values, &metrics);
    add_labels(&mut event, &metrics, rule_id);
    event
}

fn hold_event(metrics: GestureSessionMetrics, rule_id: &str) -> InputEvent {
    let mut event = InputEvent::new("touchpad.hold", "end");
    event.fingers = Some(metrics.fingers);
    add_metrics(&mut event.values, &metrics);
    add_labels(&mut event, &metrics, rule_id);
    event
}

fn add_labels(event: &mut InputEvent, metrics: &GestureSessionMetrics, rule_id: &str) {
    event
        .labels
        .insert("recognizer".to_owned(), "session-v1".to_owned());
    event
        .labels
        .insert("recognize_on".to_owned(), "touch-session-end".to_owned());
    event
        .labels
        .insert("recognition.rule_id".to_owned(), rule_id.to_owned());
    event.labels.insert(
        "tracking_complete".to_owned(),
        metrics.tracking_complete.to_string(),
    );
}

fn add_metrics(values: &mut BTreeMap<String, f64>, metrics: &GestureSessionMetrics) {
    values.insert("duration_ms".to_owned(), metrics.duration_ms);
    values.insert("dx".to_owned(), metrics.dx);
    values.insert("dy".to_owned(), metrics.dy);
    values.insert("distance".to_owned(), metrics.distance);
    values.insert("path_length".to_owned(), metrics.path_length);
    values.insert("average_velocity".to_owned(), metrics.average_velocity);
    values.insert(
        "axis_deviation_degrees".to_owned(),
        metrics.axis_deviation_degrees,
    );
    values.insert("straightness".to_owned(), metrics.straightness);
    values.insert("sample_points".to_owned(), metrics.points as f64);
}

fn best_segment_metrics(
    frames: &[TouchFrame],
    fingers: u8,
    require_complete_tracking: bool,
) -> Option<GestureSessionMetrics> {
    let mut segments = Vec::new();
    let mut builder: Option<SegmentBuilder> = None;

    for frame in frames {
        let sample = match (
            frame.fingers == fingers,
            !require_complete_tracking || frame.tracking_complete,
            frame.timestamp_micros,
            frame.centroid,
        ) {
            (true, true, Some(timestamp), Some(point)) => Some((
                timestamp,
                point,
                frame.tracking_complete,
                coordinate_contact_ids(frame),
            )),
            _ => None,
        };

        if let Some((timestamp, point, tracking_complete, contact_ids)) = sample {
            if builder
                .as_ref()
                .is_some_and(|active| active.contact_ids != contact_ids)
            {
                if let Some(metrics) = builder.take().and_then(SegmentBuilder::finish) {
                    segments.push(metrics);
                }
            }

            if let Some(active) = builder.as_mut() {
                active.push(timestamp, point, tracking_complete);
            } else {
                builder = Some(SegmentBuilder::new(
                    fingers,
                    timestamp,
                    point,
                    tracking_complete,
                    contact_ids,
                ));
            }
        } else if let Some(metrics) = builder.take().and_then(SegmentBuilder::finish) {
            segments.push(metrics);
        }
    }

    if let Some(metrics) = builder.and_then(SegmentBuilder::finish) {
        segments.push(metrics);
    }

    segments.into_iter().max_by(|left, right| {
        left.duration_ms
            .total_cmp(&right.duration_ms)
            .then_with(|| left.points.cmp(&right.points))
    })
}

fn coordinate_contact_ids(frame: &TouchFrame) -> Vec<(u16, i32)> {
    frame
        .contacts
        .iter()
        .filter(|contact| contact.x.is_some() && contact.y.is_some())
        .map(|contact| (contact.slot, contact.tracking_id))
        .collect()
}

#[derive(Debug)]
struct SegmentBuilder {
    fingers: u8,
    tracking_complete: bool,
    contact_ids: Vec<(u16, i32)>,
    points: usize,
    start_timestamp: u128,
    last_timestamp: u128,
    start_point: TouchPoint,
    last_point: TouchPoint,
    path_length: f64,
}

impl SegmentBuilder {
    fn new(
        fingers: u8,
        timestamp: u128,
        point: TouchPoint,
        tracking_complete: bool,
        contact_ids: Vec<(u16, i32)>,
    ) -> Self {
        Self {
            fingers,
            tracking_complete,
            contact_ids,
            points: 1,
            start_timestamp: timestamp,
            last_timestamp: timestamp,
            start_point: point,
            last_point: point,
            path_length: 0.0,
        }
    }

    fn push(&mut self, timestamp: u128, point: TouchPoint, tracking_complete: bool) {
        self.path_length += point_distance(self.last_point, point);
        self.tracking_complete &= tracking_complete;
        self.last_timestamp = timestamp;
        self.last_point = point;
        self.points += 1;
    }

    fn finish(self) -> Option<GestureSessionMetrics> {
        if self.points < 2 || self.last_timestamp <= self.start_timestamp {
            return None;
        }

        let duration_ms = (self.last_timestamp - self.start_timestamp) as f64 / 1_000.0;
        let dx = self.last_point.x - self.start_point.x;
        let dy = self.last_point.y - self.start_point.y;
        let distance = dx.hypot(dy);
        let average_velocity = self.path_length / (duration_ms / 1_000.0);
        let major = dx.abs().max(dy.abs());
        let minor = dx.abs().min(dy.abs());
        let axis_deviation_degrees = if major > 0.0 {
            minor.atan2(major).to_degrees()
        } else {
            0.0
        };
        let straightness = if self.path_length > 0.0 {
            distance / self.path_length
        } else {
            1.0
        };
        let direction = if dx.abs() >= dy.abs() {
            if dx >= 0.0 {
                "right"
            } else {
                "left"
            }
        } else if dy >= 0.0 {
            "down"
        } else {
            "up"
        };

        Some(GestureSessionMetrics {
            fingers: self.fingers,
            tracking_complete: self.tracking_complete,
            points: self.points,
            duration_ms,
            dx,
            dy,
            distance,
            path_length: self.path_length,
            average_velocity,
            axis_deviation_degrees,
            straightness,
            direction: direction.to_owned(),
        })
    }
}

fn point_distance(left: TouchPoint, right: TouchPoint) -> f64 {
    (right.x - left.x).hypot(right.y - left.y)
}

fn elapsed_millis(start: u128, end: u128) -> Option<f64> {
    end.checked_sub(start).map(|micros| micros as f64 / 1_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        sequence: u64,
        timestamp_ms: u128,
        phase: TouchFramePhase,
        fingers: u8,
        point: Option<(f64, f64)>,
    ) -> TouchFrame {
        TouchFrame {
            sequence,
            timestamp_micros: Some(timestamp_ms * 1_000),
            phase,
            fingers,
            tracked_contacts: usize::from(fingers),
            reported_fingers: Some(fingers),
            tracking_complete: true,
            contacts: Vec::new(),
            centroid: point.map(|(x, y)| TouchPoint { x, y }),
            delta: None,
            velocity_per_second: None,
            frame_interval_micros: None,
        }
    }

    fn recognize(points: &[(u128, f64, f64)]) -> Vec<InputEvent> {
        let mut recognizer = GestureRecognizer::new(RecognizerConfig::default()).unwrap();
        let _ = recognizer.push(&frame(
            1,
            0,
            TouchFramePhase::Begin,
            1,
            Some((500.0, 700.0)),
        ));
        for (index, (timestamp, x, y)) in points.iter().enumerate() {
            let _ = recognizer.push(&frame(
                index as u64 + 2,
                *timestamp,
                TouchFramePhase::Update,
                3,
                Some((*x, *y)),
            ));
        }
        recognizer.push(&frame(
            points.len() as u64 + 2,
            points.last().map_or(1_000, |point| point.0 + 10),
            TouchFramePhase::End,
            0,
            None,
        ))
    }

    fn three_finger_drag_rule() -> crate::DragRuleConfig {
        crate::DragRuleConfig {
            id: "three-finger-drag".to_owned(),
            enabled: true,
            fingers: 3,
            min_hold_duration_ms: 300.0,
            max_hold_distance: 20.0,
            min_drag_distance: 5.0,
            require_complete_tracking: true,
        }
    }

    #[test]
    fn recognizes_three_finger_up_swipe() {
        let events = recognize(&[
            (100, 500.0, 700.0),
            (300, 505.0, 500.0),
            (500, 510.0, 300.0),
        ]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].family, "touchpad.swipe");
        assert_eq!(events[0].direction.as_deref(), Some("up"));
        assert_eq!(events[0].fingers, Some(3));
        assert_eq!(
            events[0]
                .labels
                .get("recognition.rule_id")
                .map(String::as_str),
            Some("three-finger-swipe")
        );
    }

    #[test]
    fn rejects_slow_move_even_when_distance_is_large() {
        let events = recognize(&[
            (100, 100.0, 100.0),
            (900, 230.0, 100.0),
            (1_700, 360.0, 100.0),
        ]);
        assert!(events.is_empty());
    }

    #[test]
    fn rejects_small_fast_move_even_when_velocity_is_high() {
        let events = recognize(&[(100, 100.0, 100.0), (250, 250.0, 100.0)]);
        assert!(events.is_empty());
    }

    #[test]
    fn rejects_diagonal_motion_outside_axis_tolerance() {
        let events = recognize(&[(100, 100.0, 100.0), (500, 400.0, 400.0)]);
        assert!(events.is_empty());
    }

    #[test]
    fn recognizes_stationary_three_finger_hold() {
        let events = recognize(&[
            (100, 500.0, 500.0),
            (500, 504.0, 497.0),
            (900, 506.0, 501.0),
        ]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].family, "touchpad.hold");
        assert_eq!(events[0].phase, "end");
    }

    #[test]
    fn ignores_shorter_transition_segment() {
        let mut recognizer = GestureRecognizer::new(RecognizerConfig::default()).unwrap();
        let _ = recognizer.push(&frame(1, 0, TouchFramePhase::Begin, 1, Some((0.0, 0.0))));
        let _ = recognizer.push(&frame(2, 50, TouchFramePhase::Update, 3, Some((0.0, 0.0))));
        let _ = recognizer.push(&frame(
            3,
            100,
            TouchFramePhase::Update,
            3,
            Some((20.0, 0.0)),
        ));
        let _ = recognizer.push(&frame(
            4,
            120,
            TouchFramePhase::Update,
            2,
            Some((20.0, 0.0)),
        ));
        let _ = recognizer.push(&frame(
            5,
            200,
            TouchFramePhase::Update,
            3,
            Some((0.0, 500.0)),
        ));
        let _ = recognizer.push(&frame(
            6,
            400,
            TouchFramePhase::Update,
            3,
            Some((250.0, 500.0)),
        ));
        let events = recognizer.push(&frame(7, 420, TouchFramePhase::End, 0, None));

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].direction.as_deref(), Some("right"));
    }

    #[test]
    fn recognizes_four_finger_rule_alongside_three_finger_rule() {
        let mut config = RecognizerConfig::default();
        config.recognition.swipes.push(crate::SwipeRuleConfig {
            id: "four-finger-swipe".to_owned(),
            fingers: 4,
            ..crate::SwipeRuleConfig::default()
        });
        let mut recognizer = GestureRecognizer::new(config).unwrap();
        let _ = recognizer.push(&frame(1, 0, TouchFramePhase::Begin, 1, Some((0.0, 0.0))));
        let _ = recognizer.push(&frame(
            2,
            100,
            TouchFramePhase::Update,
            4,
            Some((100.0, 500.0)),
        ));
        let _ = recognizer.push(&frame(
            3,
            300,
            TouchFramePhase::Update,
            4,
            Some((105.0, 250.0)),
        ));
        let events = recognizer.push(&frame(4, 320, TouchFramePhase::End, 0, None));

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].fingers, Some(4));
        assert_eq!(
            events[0]
                .labels
                .get("recognition.rule_id")
                .map(String::as_str),
            Some("four-finger-swipe")
        );
    }

    #[test]
    fn recognizes_five_finger_synthetic_rule() {
        let mut config = RecognizerConfig::default();
        config.recognition.swipes = vec![crate::SwipeRuleConfig {
            id: "five-finger-swipe".to_owned(),
            fingers: 5,
            ..crate::SwipeRuleConfig::default()
        }];
        let mut recognizer = GestureRecognizer::new(config).unwrap();
        let _ = recognizer.push(&frame(1, 0, TouchFramePhase::Begin, 1, Some((0.0, 0.0))));
        let _ = recognizer.push(&frame(
            2,
            100,
            TouchFramePhase::Update,
            5,
            Some((100.0, 100.0)),
        ));
        let _ = recognizer.push(&frame(
            3,
            350,
            TouchFramePhase::Update,
            5,
            Some((400.0, 105.0)),
        ));
        let events = recognizer.push(&frame(4, 370, TouchFramePhase::End, 0, None));

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].fingers, Some(5));
        assert_eq!(events[0].direction.as_deref(), Some("right"));
    }

    #[test]
    fn longest_matching_rule_wins_a_multi_finger_conflict() {
        let mut config = RecognizerConfig::default();
        config.recognition.swipes.push(crate::SwipeRuleConfig {
            id: "four-finger-swipe".to_owned(),
            fingers: 4,
            ..crate::SwipeRuleConfig::default()
        });
        let mut recognizer = GestureRecognizer::new(config).unwrap();
        let _ = recognizer.push(&frame(1, 0, TouchFramePhase::Begin, 1, Some((0.0, 0.0))));
        let _ = recognizer.push(&frame(2, 100, TouchFramePhase::Update, 3, Some((0.0, 0.0))));
        let _ = recognizer.push(&frame(
            3,
            200,
            TouchFramePhase::Update,
            3,
            Some((250.0, 0.0)),
        ));
        let _ = recognizer.push(&frame(
            4,
            220,
            TouchFramePhase::Update,
            2,
            Some((250.0, 0.0)),
        ));
        let _ = recognizer.push(&frame(5, 300, TouchFramePhase::Update, 4, Some((0.0, 0.0))));
        let _ = recognizer.push(&frame(
            6,
            600,
            TouchFramePhase::Update,
            4,
            Some((300.0, 0.0)),
        ));
        let events = recognizer.push(&frame(7, 620, TouchFramePhase::End, 0, None));

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].fingers, Some(4));
        assert_eq!(
            events[0]
                .labels
                .get("recognition.rule_id")
                .map(String::as_str),
            Some("four-finger-swipe")
        );
    }

    #[test]
    fn complete_tracking_rule_rejects_partial_coordinates() {
        let mut config = RecognizerConfig::default();
        config.recognition.holds.clear();
        let mut recognizer = GestureRecognizer::new(config).unwrap();
        let _ = recognizer.push(&frame(1, 0, TouchFramePhase::Begin, 1, Some((0.0, 0.0))));
        for (sequence, timestamp, x) in [(2, 100, 100.0), (3, 300, 350.0)] {
            let mut partial = frame(
                sequence,
                timestamp,
                TouchFramePhase::Update,
                3,
                Some((x, 100.0)),
            );
            partial.tracked_contacts = 2;
            partial.tracking_complete = false;
            let _ = recognizer.push(&partial);
        }
        let events = recognizer.push(&frame(4, 320, TouchFramePhase::End, 0, None));

        assert!(events.is_empty());
    }

    #[test]
    fn legacy_compatible_rule_can_use_partial_coordinates() {
        let config: RecognizerConfig = toml::from_str(
            r#"
                [recognition.three_finger_swipe]
                enabled = true

                [recognition.three_finger_hold]
                enabled = false
            "#,
        )
        .unwrap();
        let mut recognizer = GestureRecognizer::new(config).unwrap();
        let _ = recognizer.push(&frame(1, 0, TouchFramePhase::Begin, 1, Some((0.0, 0.0))));
        for (sequence, timestamp, x) in [(2, 100, 100.0), (3, 300, 350.0)] {
            let mut partial = frame(
                sequence,
                timestamp,
                TouchFramePhase::Update,
                3,
                Some((x, 100.0)),
            );
            partial.tracked_contacts = 2;
            partial.tracking_complete = false;
            let _ = recognizer.push(&partial);
        }
        let events = recognizer.push(&frame(4, 320, TouchFramePhase::End, 0, None));

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0]
                .labels
                .get("tracking_complete")
                .map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn emits_continuous_drag_lifecycle_after_hold_then_move() {
        let mut config = RecognizerConfig::default();
        config.recognition.drags = vec![three_finger_drag_rule()];
        let mut recognizer = GestureRecognizer::new(config).unwrap();

        assert!(recognizer
            .push(&frame(
                1,
                0,
                TouchFramePhase::Begin,
                1,
                Some((100.0, 100.0))
            ))
            .is_empty());
        assert!(recognizer
            .push(&frame(
                2,
                100,
                TouchFramePhase::Update,
                3,
                Some((100.0, 100.0))
            ))
            .is_empty());
        assert!(recognizer
            .push(&frame(
                3,
                450,
                TouchFramePhase::Update,
                3,
                Some((102.0, 100.0))
            ))
            .is_empty());

        let begin = recognizer.push(&frame(
            4,
            500,
            TouchFramePhase::Update,
            3,
            Some((112.0, 100.0)),
        ));
        assert_eq!(begin.len(), 1);
        assert_eq!(begin[0].family, "touchpad.drag");
        assert_eq!(begin[0].phase, "begin");
        assert_eq!(begin[0].values.get("total_dx"), Some(&10.0));
        assert_eq!(
            begin[0]
                .labels
                .get("recognition.rule_id")
                .map(String::as_str),
            Some("three-finger-drag")
        );

        let update = recognizer.push(&frame(
            5,
            550,
            TouchFramePhase::Update,
            3,
            Some((130.0, 105.0)),
        ));
        assert_eq!(update.len(), 1);
        assert_eq!(update[0].phase, "update");
        assert_eq!(update[0].values.get("dx"), Some(&18.0));
        assert_eq!(update[0].values.get("dy"), Some(&5.0));
        assert_eq!(update[0].values.get("total_dx"), Some(&28.0));

        let end = recognizer.push(&frame(6, 600, TouchFramePhase::End, 0, None));
        assert_eq!(end.len(), 1);
        assert_eq!(end[0].family, "touchpad.drag");
        assert_eq!(end[0].phase, "end");
        assert_eq!(end[0].values.get("total_dx"), Some(&28.0));
    }

    #[test]
    fn armed_drag_without_motion_remains_a_hold() {
        let mut config = RecognizerConfig::default();
        config.recognition.drags = vec![three_finger_drag_rule()];
        let mut recognizer = GestureRecognizer::new(config).unwrap();
        let _ = recognizer.push(&frame(
            1,
            0,
            TouchFramePhase::Begin,
            1,
            Some((100.0, 100.0)),
        ));
        for (sequence, timestamp) in [(2, 100), (3, 500), (4, 900)] {
            assert!(recognizer
                .push(&frame(
                    sequence,
                    timestamp,
                    TouchFramePhase::Update,
                    3,
                    Some((100.0, 100.0)),
                ))
                .is_empty());
        }
        let events = recognizer.push(&frame(5, 920, TouchFramePhase::End, 0, None));

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].family, "touchpad.hold");
    }

    #[test]
    fn movement_before_hold_still_classifies_as_swipe() {
        let mut config = RecognizerConfig::default();
        config.recognition.drags = vec![three_finger_drag_rule()];
        let mut recognizer = GestureRecognizer::new(config).unwrap();
        let _ = recognizer.push(&frame(1, 0, TouchFramePhase::Begin, 1, Some((0.0, 0.0))));
        let _ = recognizer.push(&frame(2, 100, TouchFramePhase::Update, 3, Some((0.0, 0.0))));
        let _ = recognizer.push(&frame(
            3,
            250,
            TouchFramePhase::Update,
            3,
            Some((250.0, 0.0)),
        ));
        let events = recognizer.push(&frame(4, 270, TouchFramePhase::End, 0, None));

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].family, "touchpad.swipe");
    }

    #[test]
    fn active_drag_cancels_when_finger_count_changes() {
        let mut config = RecognizerConfig::default();
        config.recognition.drags = vec![three_finger_drag_rule()];
        let mut recognizer = GestureRecognizer::new(config).unwrap();
        let _ = recognizer.push(&frame(
            1,
            0,
            TouchFramePhase::Begin,
            3,
            Some((100.0, 100.0)),
        ));
        let _ = recognizer.push(&frame(
            2,
            350,
            TouchFramePhase::Update,
            3,
            Some((100.0, 100.0)),
        ));
        let begin = recognizer.push(&frame(
            3,
            400,
            TouchFramePhase::Update,
            3,
            Some((110.0, 100.0)),
        ));
        assert_eq!(begin[0].phase, "begin");

        let cancel = recognizer.push(&frame(
            4,
            450,
            TouchFramePhase::Update,
            2,
            Some((110.0, 100.0)),
        ));
        assert_eq!(cancel.len(), 1);
        assert_eq!(cancel[0].phase, "cancel");

        assert!(recognizer
            .push(&frame(5, 500, TouchFramePhase::End, 0, None))
            .is_empty());
    }

    #[test]
    fn active_drag_cancels_when_required_tracking_becomes_partial() {
        let mut config = RecognizerConfig::default();
        config.recognition.drags = vec![three_finger_drag_rule()];
        let mut recognizer = GestureRecognizer::new(config).unwrap();
        let _ = recognizer.push(&frame(
            1,
            0,
            TouchFramePhase::Begin,
            3,
            Some((100.0, 100.0)),
        ));
        let _ = recognizer.push(&frame(
            2,
            350,
            TouchFramePhase::Update,
            3,
            Some((100.0, 100.0)),
        ));
        let begin = recognizer.push(&frame(
            3,
            400,
            TouchFramePhase::Update,
            3,
            Some((110.0, 100.0)),
        ));
        assert_eq!(begin[0].phase, "begin");

        let mut partial = frame(4, 450, TouchFramePhase::Update, 3, Some((115.0, 100.0)));
        partial.tracked_contacts = 2;
        partial.tracking_complete = false;
        let cancel = recognizer.push(&partial);

        assert_eq!(cancel.len(), 1);
        assert_eq!(cancel[0].phase, "cancel");
    }

    #[test]
    fn selects_four_finger_drag_rule_from_multiple_counts() {
        let mut four_finger_rule = three_finger_drag_rule();
        four_finger_rule.id = "four-finger-drag".to_owned();
        four_finger_rule.fingers = 4;
        let mut config = RecognizerConfig::default();
        config.recognition.drags = vec![three_finger_drag_rule(), four_finger_rule];
        let mut recognizer = GestureRecognizer::new(config).unwrap();
        let _ = recognizer.push(&frame(
            1,
            0,
            TouchFramePhase::Begin,
            4,
            Some((100.0, 100.0)),
        ));
        let _ = recognizer.push(&frame(
            2,
            350,
            TouchFramePhase::Update,
            4,
            Some((100.0, 100.0)),
        ));
        let begin = recognizer.push(&frame(
            3,
            400,
            TouchFramePhase::Update,
            4,
            Some((110.0, 100.0)),
        ));

        assert_eq!(begin.len(), 1);
        assert_eq!(begin[0].fingers, Some(4));
        assert_eq!(
            begin[0]
                .labels
                .get("recognition.rule_id")
                .map(String::as_str),
            Some("four-finger-drag")
        );
    }
}
