use std::collections::BTreeMap;

use anyhow::Result;
use gesture_core::InputEvent;
use gesture_device::{TouchFrame, TouchFramePhase, TouchPoint};
use serde::{Deserialize, Serialize};

use crate::RecognizerConfig;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GestureSessionMetrics {
    pub fingers: u8,
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
}

impl GestureRecognizer {
    pub fn new(config: RecognizerConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            session: Vec::new(),
        })
    }

    pub fn config(&self) -> &RecognizerConfig {
        &self.config
    }

    /// Consume one normalized touch frame and return zero or more action-agnostic
    /// gesture events. v0.4 classifies completed touch sessions only.
    pub fn push(&mut self, frame: &TouchFrame) -> Vec<InputEvent> {
        if frame.phase == TouchFramePhase::Begin {
            self.session.clear();
        }

        if self.session.is_empty() && frame.fingers == 0 {
            return Vec::new();
        }

        self.session.push(frame.clone());

        if frame.phase != TouchFramePhase::End {
            return Vec::new();
        }

        let events = self.classify_completed_session();
        self.session.clear();
        events
    }

    fn classify_completed_session(&self) -> Vec<InputEvent> {
        let swipe = &self.config.recognition.three_finger_swipe;
        let swipe_metrics = if swipe.enabled {
            best_segment_metrics(&self.session, swipe.fingers)
        } else {
            None
        };
        let swipe_metrics = swipe_metrics.filter(|metrics| {
            metrics.distance >= swipe.min_distance
                && metrics.average_velocity >= swipe.min_average_velocity
                && metrics.duration_ms <= swipe.max_duration_ms
                && metrics.axis_deviation_degrees <= swipe.max_axis_deviation_degrees
        });
        if let Some(metrics) = swipe_metrics {
            return vec![swipe_event(metrics)];
        }

        let hold = &self.config.recognition.three_finger_hold;
        let hold_metrics = if hold.enabled {
            best_segment_metrics(&self.session, hold.fingers)
        } else {
            None
        };
        let hold_metrics = hold_metrics.filter(|metrics| {
            metrics.duration_ms >= hold.min_duration_ms && metrics.distance <= hold.max_net_distance
        });
        if let Some(metrics) = hold_metrics {
            return vec![hold_event(metrics)];
        }

        Vec::new()
    }
}

fn swipe_event(metrics: GestureSessionMetrics) -> InputEvent {
    let mut event = InputEvent::new("touchpad.swipe", "end");
    event.fingers = Some(metrics.fingers);
    event.direction = Some(metrics.direction.clone());
    add_metrics(&mut event.values, &metrics);
    event
        .labels
        .insert("recognizer".to_owned(), "session-v1".to_owned());
    event
        .labels
        .insert("recognize_on".to_owned(), "touch-session-end".to_owned());
    event
}

fn hold_event(metrics: GestureSessionMetrics) -> InputEvent {
    let mut event = InputEvent::new("touchpad.hold", "end");
    event.fingers = Some(metrics.fingers);
    add_metrics(&mut event.values, &metrics);
    event
        .labels
        .insert("recognizer".to_owned(), "session-v1".to_owned());
    event
        .labels
        .insert("recognize_on".to_owned(), "touch-session-end".to_owned());
    event
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

fn best_segment_metrics(frames: &[TouchFrame], fingers: u8) -> Option<GestureSessionMetrics> {
    let mut segments = Vec::new();
    let mut builder: Option<SegmentBuilder> = None;

    for frame in frames {
        let sample = match (
            frame.fingers == fingers,
            frame.timestamp_micros,
            frame.centroid,
        ) {
            (true, Some(timestamp), Some(point)) => Some((timestamp, point)),
            _ => None,
        };

        if let Some((timestamp, point)) = sample {
            if let Some(active) = builder.as_mut() {
                active.push(timestamp, point);
            } else {
                builder = Some(SegmentBuilder::new(fingers, timestamp, point));
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

#[derive(Debug)]
struct SegmentBuilder {
    fingers: u8,
    points: usize,
    start_timestamp: u128,
    last_timestamp: u128,
    start_point: TouchPoint,
    last_point: TouchPoint,
    path_length: f64,
}

impl SegmentBuilder {
    fn new(fingers: u8, timestamp: u128, point: TouchPoint) -> Self {
        Self {
            fingers,
            points: 1,
            start_timestamp: timestamp,
            last_timestamp: timestamp,
            start_point: point,
            last_point: point,
            path_length: 0.0,
        }
    }

    fn push(&mut self, timestamp: u128, point: TouchPoint) {
        self.path_length += point_distance(self.last_point, point);
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
}
