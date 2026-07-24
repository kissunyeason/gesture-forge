use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use evdev::{
    uinput::VirtualDevice, AttributeSet, EventType, InputEvent as EvdevInputEvent, KeyCode,
    RelativeAxisCode,
};
use gesture_core::{ActionOutcome, ActionProvider, ActionSpec, InputEvent};
use serde::Deserialize;

use crate::uinput_keyboard::{parse_key_chord, UinputKeyboardRuntime};

const DEVICE_NAME: &str = "GestureForge Virtual Pointer";

pub struct UinputPointerProvider {
    runtime: Mutex<PointerRuntime>,
    keyboard: Mutex<UinputKeyboardRuntime>,
}

impl UinputPointerProvider {
    pub fn new() -> Self {
        Self {
            runtime: Mutex::new(PointerRuntime::default()),
            keyboard: Mutex::new(UinputKeyboardRuntime::default()),
        }
    }
}

impl Default for UinputPointerProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for UinputPointerProvider {
    fn drop(&mut self) {
        if let Ok(runtime) = self.runtime.get_mut() {
            let _ = runtime.release_all_buttons();
        }
        if let Ok(keyboard) = self.keyboard.get_mut() {
            let _ = keyboard.release_all_keys();
        }
    }
}

#[async_trait]
impl ActionProvider for UinputPointerProvider {
    fn name(&self) -> &'static str {
        "uinput"
    }

    fn validate(&self, spec: &ActionSpec) -> Result<()> {
        match spec.action.as_str() {
            "drag" => {
                let params = parse_params(spec)?;
                params.validate()
            }
            "key-chord" => parse_key_chord(spec).map(|_| ()),
            other => bail!("unknown uinput action {other:?}"),
        }
    }

    async fn execute(&self, spec: &ActionSpec, event: &InputEvent) -> Result<ActionOutcome> {
        self.validate(spec)?;
        match spec.action.as_str() {
            "drag" => self.execute_drag(spec, event),
            "key-chord" => self.execute_key_chord(spec),
            _ => unreachable!("validated above"),
        }
    }
}

impl UinputPointerProvider {
    fn execute_drag(&self, spec: &ActionSpec, event: &InputEvent) -> Result<ActionOutcome> {
        if event.family != "touchpad.drag" {
            bail!(
                "uinput.drag requires touchpad.drag events, received {:?}",
                event.family
            );
        }

        let params = parse_params(spec)?;
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| anyhow::anyhow!("uinput pointer state lock was poisoned"))?;
        let transition = runtime.state.plan(event, &params)?;

        if !transition.commands.is_empty() {
            if let Err(error) = runtime.emit(&transition.commands) {
                let release_result = runtime.release_all_buttons();
                runtime.state = PointerDragState::default();
                if let Err(release_error) = release_result {
                    return Err(error).context(format!(
                        "uinput event emission failed; emergency button release also failed: {release_error}"
                    ));
                }
                return Err(error).context(
                    "uinput event emission failed; emergency button release was attempted",
                );
            }
        }
        runtime.state = transition.next_state;

        Ok(ActionOutcome::success(
            spec,
            Some(format!(
                "virtual pointer handled drag phase {}",
                event.phase
            )),
        ))
    }

    fn execute_key_chord(&self, spec: &ActionSpec) -> Result<ActionOutcome> {
        let chord = parse_key_chord(spec)?;
        let mut keyboard = self
            .keyboard
            .lock()
            .map_err(|_| anyhow::anyhow!("uinput keyboard state lock was poisoned"))?;
        keyboard.tap_chord(&chord.keys, chord.hold)?;
        Ok(ActionOutcome::success(
            spec,
            Some("virtual keyboard sent key chord".to_owned()),
        ))
    }
}

fn parse_params(spec: &ActionSpec) -> Result<DragParams> {
    serde_json::from_value(spec.params.clone())
        .context("uinput.drag expects { button?, scale?, max_delta? }")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DragParams {
    button: MouseButton,
    scale: f64,
    max_delta: i32,
}

impl Default for DragParams {
    fn default() -> Self {
        Self {
            button: MouseButton::Left,
            scale: 1.0,
            max_delta: 200,
        }
    }
}

impl DragParams {
    fn validate(&self) -> Result<()> {
        if !self.scale.is_finite() || self.scale <= 0.0 || self.scale > 100.0 {
            bail!("uinput.drag scale must be finite, greater than 0, and at most 100");
        }
        if !(1..=10_000).contains(&self.max_delta) {
            bail!("uinput.drag max_delta must be between 1 and 10000");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum MouseButton {
    #[default]
    Left,
    Middle,
    Right,
}

impl MouseButton {
    fn key_code(self) -> KeyCode {
        match self {
            Self::Left => KeyCode::BTN_LEFT,
            Self::Middle => KeyCode::BTN_MIDDLE,
            Self::Right => KeyCode::BTN_RIGHT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PointerCommand {
    Button { button: MouseButton, down: bool },
    Move { x: i32, y: i32 },
}

#[derive(Debug, Clone, Default)]
struct PointerDragState {
    active_stream_id: Option<String>,
    active_button: Option<MouseButton>,
    residual_x: f64,
    residual_y: f64,
    last_event_id: Option<String>,
}

struct PointerTransition {
    commands: Vec<PointerCommand>,
    next_state: PointerDragState,
}

impl PointerDragState {
    fn plan(&self, event: &InputEvent, params: &DragParams) -> Result<PointerTransition> {
        let event_id = event.id.to_string();
        if self.last_event_id.as_deref() == Some(event_id.as_str()) {
            return Ok(PointerTransition {
                commands: Vec::new(),
                next_state: self.clone(),
            });
        }

        match event.phase.as_str() {
            "begin" => self.plan_begin(event, params, event_id),
            "update" => self.plan_update(event, params, event_id),
            "end" | "cancel" => self.plan_release(event, event_id),
            other => bail!("uinput.drag does not support phase {other:?}"),
        }
    }

    fn plan_begin(
        &self,
        event: &InputEvent,
        params: &DragParams,
        event_id: String,
    ) -> Result<PointerTransition> {
        let stream_id = drag_stream_id(event)?;
        if self.active_stream_id.as_deref() == Some(stream_id) {
            return Ok(self.noop_with_event(event_id));
        }

        let mut commands = Vec::new();
        if let Some(button) = self.active_button {
            commands.push(PointerCommand::Button {
                button,
                down: false,
            });
        }
        commands.push(PointerCommand::Button {
            button: params.button,
            down: true,
        });

        let mut next_state = Self {
            active_stream_id: Some(stream_id.to_owned()),
            active_button: Some(params.button),
            last_event_id: Some(event_id),
            ..Self::default()
        };
        if let Some(movement) = next_state.scaled_movement(event, params)? {
            commands.push(movement);
        }

        Ok(PointerTransition {
            commands,
            next_state,
        })
    }

    fn plan_update(
        &self,
        event: &InputEvent,
        params: &DragParams,
        event_id: String,
    ) -> Result<PointerTransition> {
        let Some(active_button) = self.active_button else {
            bail!("uinput.drag received update before begin");
        };
        let active_stream_id = self
            .active_stream_id
            .as_deref()
            .context("uinput.drag active state is missing its stream id")?;
        let stream_id = drag_stream_id(event)?;
        if active_stream_id != stream_id {
            bail!(
                "uinput.drag update belongs to stream {stream_id:?}, but {active_stream_id:?} is active"
            );
        }
        if active_button != params.button {
            bail!("uinput.drag button cannot change during an active drag");
        }

        let mut next_state = self.clone();
        next_state.last_event_id = Some(event_id);
        let commands = next_state
            .scaled_movement(event, params)?
            .into_iter()
            .collect();
        Ok(PointerTransition {
            commands,
            next_state,
        })
    }

    fn plan_release(&self, event: &InputEvent, _event_id: String) -> Result<PointerTransition> {
        let Some(active_stream_id) = self.active_stream_id.as_deref() else {
            return Ok(PointerTransition {
                commands: Vec::new(),
                next_state: self.clone(),
            });
        };
        let stream_id = drag_stream_id(event)?;
        if active_stream_id != stream_id {
            return Ok(PointerTransition {
                commands: Vec::new(),
                next_state: self.clone(),
            });
        }

        let commands = self
            .active_button
            .map(|button| PointerCommand::Button {
                button,
                down: false,
            })
            .into_iter()
            .collect();
        Ok(PointerTransition {
            commands,
            next_state: Self::default(),
        })
    }

    fn noop_with_event(&self, event_id: String) -> PointerTransition {
        let mut next_state = self.clone();
        next_state.last_event_id = Some(event_id);
        PointerTransition {
            commands: Vec::new(),
            next_state,
        }
    }

    fn scaled_movement(
        &mut self,
        event: &InputEvent,
        params: &DragParams,
    ) -> Result<Option<PointerCommand>> {
        let dx = event
            .values
            .get("dx")
            .copied()
            .context("touchpad.drag event is missing dx")?;
        let dy = event
            .values
            .get("dy")
            .copied()
            .context("touchpad.drag event is missing dy")?;
        if !dx.is_finite() || !dy.is_finite() {
            bail!("touchpad.drag dx and dy must be finite");
        }

        let (x, residual_x) = scale_axis(dx, params.scale, self.residual_x, params.max_delta);
        let (y, residual_y) = scale_axis(dy, params.scale, self.residual_y, params.max_delta);
        self.residual_x = residual_x;
        self.residual_y = residual_y;

        Ok((x != 0 || y != 0).then_some(PointerCommand::Move { x, y }))
    }
}

fn drag_stream_id(event: &InputEvent) -> Result<&str> {
    event
        .labels
        .get("recognition.stream_id")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .context("touchpad.drag event is missing recognition.stream_id")
}

fn scale_axis(value: f64, scale: f64, residual: f64, max_delta: i32) -> (i32, f64) {
    let scaled = value * scale + residual;
    let rounded = scaled.round();
    let clamped = rounded.clamp(f64::from(-max_delta), f64::from(max_delta)) as i32;
    let next_residual = if rounded == f64::from(clamped) {
        scaled - rounded
    } else {
        0.0
    };
    (clamped, next_residual)
}

#[derive(Default)]
struct PointerRuntime {
    state: PointerDragState,
    device: Option<VirtualDevice>,
}

impl PointerRuntime {
    fn emit(&mut self, commands: &[PointerCommand]) -> Result<()> {
        for events in command_event_packets(commands) {
            self.device()?
                .emit(&events)
                .context("failed to emit virtual pointer command frame")?;
        }
        Ok(())
    }

    fn device(&mut self) -> Result<&mut VirtualDevice> {
        if self.device.is_none() {
            self.device = Some(create_virtual_pointer()?);
        }
        Ok(self.device.as_mut().expect("device initialized above"))
    }

    fn release_all_buttons(&mut self) -> Result<()> {
        let Some(device) = self.device.as_mut() else {
            return Ok(());
        };
        let events: Vec<_> = [MouseButton::Left, MouseButton::Middle, MouseButton::Right]
            .into_iter()
            .map(|button| EvdevInputEvent::new_now(EventType::KEY.0, button.key_code().code(), 0))
            .collect();
        device
            .emit(&events)
            .context("failed to release virtual pointer buttons")
    }
}

fn create_virtual_pointer() -> Result<VirtualDevice> {
    let keys =
        AttributeSet::from_iter([KeyCode::BTN_LEFT, KeyCode::BTN_MIDDLE, KeyCode::BTN_RIGHT]);
    let axes = AttributeSet::from_iter([RelativeAxisCode::REL_X, RelativeAxisCode::REL_Y]);
    VirtualDevice::builder()
        .context("failed to open /dev/uinput")?
        .name(DEVICE_NAME)
        .with_keys(&keys)
        .context("failed to configure virtual pointer buttons")?
        .with_relative_axes(&axes)
        .context("failed to configure virtual pointer axes")?
        .build()
        .context("failed to create virtual pointer")
}

fn command_event_packets(commands: &[PointerCommand]) -> Vec<Vec<EvdevInputEvent>> {
    commands
        .iter()
        .map(command_events)
        .filter(|events| !events.is_empty())
        .collect()
}

fn command_events(command: &PointerCommand) -> Vec<EvdevInputEvent> {
    match *command {
        PointerCommand::Button { button, down } => vec![EvdevInputEvent::new_now(
            EventType::KEY.0,
            button.key_code().code(),
            i32::from(down),
        )],
        PointerCommand::Move { x, y } => {
            [(RelativeAxisCode::REL_X, x), (RelativeAxisCode::REL_Y, y)]
                .into_iter()
                .filter(|(_, value)| *value != 0)
                .map(|(axis, value)| EvdevInputEvent::new_now(EventType::RELATIVE.0, axis.0, value))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(phase: &str, dx: f64, dy: f64) -> InputEvent {
        event_for_stream("stream-a", phase, dx, dy)
    }

    fn event_for_stream(stream_id: &str, phase: &str, dx: f64, dy: f64) -> InputEvent {
        let mut event = InputEvent::new("touchpad.drag", phase);
        event.values.insert("dx".to_owned(), dx);
        event.values.insert("dy".to_owned(), dy);
        event
            .labels
            .insert("recognition.stream_id".to_owned(), stream_id.to_owned());
        event
    }

    #[test]
    fn button_press_and_initial_motion_are_separate_uinput_frames() {
        let commands = [
            PointerCommand::Button {
                button: MouseButton::Left,
                down: true,
            },
            PointerCommand::Move { x: 5, y: -2 },
        ];
        let packets = command_event_packets(&commands);

        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].len(), 1);
        assert_eq!(packets[0][0].event_type(), EventType::KEY);
        assert_eq!(packets[0][0].code(), KeyCode::BTN_LEFT.code());
        assert_eq!(packets[0][0].value(), 1);
        assert_eq!(packets[1].len(), 2);
        assert!(packets[1]
            .iter()
            .all(|event| event.event_type() == EventType::RELATIVE));
    }

    #[test]
    fn plans_press_move_update_and_release() {
        let params = DragParams {
            scale: 0.5,
            ..DragParams::default()
        };
        let begin = PointerDragState::default()
            .plan(&event("begin", 10.0, -4.0), &params)
            .unwrap();
        assert_eq!(
            begin.commands,
            vec![
                PointerCommand::Button {
                    button: MouseButton::Left,
                    down: true,
                },
                PointerCommand::Move { x: 5, y: -2 },
            ]
        );

        let update = begin
            .next_state
            .plan(&event("update", 6.0, 8.0), &params)
            .unwrap();
        assert_eq!(update.commands, vec![PointerCommand::Move { x: 3, y: 4 }]);

        let end = update
            .next_state
            .plan(&event("end", 0.0, 0.0), &params)
            .unwrap();
        assert_eq!(
            end.commands,
            vec![PointerCommand::Button {
                button: MouseButton::Left,
                down: false,
            }]
        );
        assert!(end.next_state.active_button.is_none());
    }

    #[test]
    fn cancel_is_an_idempotent_release() {
        let params = DragParams::default();
        let inactive = PointerDragState::default();
        assert!(inactive
            .plan(&event("cancel", 0.0, 0.0), &params)
            .unwrap()
            .commands
            .is_empty());

        let active = inactive
            .plan(&event("begin", 0.0, 0.0), &params)
            .unwrap()
            .next_state;
        assert_eq!(
            active
                .plan(&event("cancel", 0.0, 0.0), &params)
                .unwrap()
                .commands,
            vec![PointerCommand::Button {
                button: MouseButton::Left,
                down: false,
            }]
        );
    }

    #[test]
    fn update_before_begin_is_rejected() {
        assert!(PointerDragState::default()
            .plan(&event("update", 1.0, 1.0), &DragParams::default())
            .is_err());
    }

    #[test]
    fn fractional_movement_is_preserved_as_residual() {
        let params = DragParams {
            scale: 0.25,
            ..DragParams::default()
        };
        let begin = PointerDragState::default()
            .plan(&event("begin", 1.0, 0.0), &params)
            .unwrap();
        assert_eq!(begin.commands.len(), 1);
        let update = begin
            .next_state
            .plan(&event("update", 1.0, 0.0), &params)
            .unwrap();
        assert_eq!(update.commands, vec![PointerCommand::Move { x: 1, y: 0 }]);
    }

    #[test]
    fn movement_is_clamped_without_backlog() {
        let params = DragParams {
            max_delta: 100,
            ..DragParams::default()
        };
        let begin = PointerDragState::default()
            .plan(&event("begin", 500.0, -500.0), &params)
            .unwrap();
        assert_eq!(
            begin.commands.last(),
            Some(&PointerCommand::Move { x: 100, y: -100 })
        );
        assert_eq!(begin.next_state.residual_x, 0.0);
        assert_eq!(begin.next_state.residual_y, 0.0);
    }

    #[test]
    fn duplicate_events_are_idempotent() {
        let params = DragParams::default();
        let begin_event = event("begin", 10.0, 0.0);
        let begin = PointerDragState::default()
            .plan(&begin_event, &params)
            .unwrap();
        let duplicate = begin.next_state.plan(&begin_event, &params).unwrap();
        assert!(duplicate.commands.is_empty());
        assert_eq!(
            duplicate.next_state.active_stream_id.as_deref(),
            Some("stream-a")
        );
    }

    #[test]
    fn stale_cancel_does_not_release_a_newer_stream() {
        let params = DragParams::default();
        let active = PointerDragState::default()
            .plan(&event_for_stream("stream-b", "begin", 0.0, 0.0), &params)
            .unwrap()
            .next_state;
        let stale = active
            .plan(&event_for_stream("stream-a", "cancel", 0.0, 0.0), &params)
            .unwrap();
        assert!(stale.commands.is_empty());
        assert_eq!(
            stale.next_state.active_stream_id.as_deref(),
            Some("stream-b")
        );
    }

    #[test]
    fn validates_action_parameters_without_opening_uinput() {
        let provider = UinputPointerProvider::new();
        let valid = ActionSpec {
            provider: "uinput".to_owned(),
            action: "drag".to_owned(),
            params: serde_json::json!({
                "button": "left",
                "scale": 0.5,
                "max_delta": 100
            }),
            on_error: gesture_core::ErrorPolicy::Continue,
        };
        provider.validate(&valid).unwrap();

        let mut invalid = valid;
        invalid.params = serde_json::json!({ "scale": 0.0 });
        assert!(provider.validate(&invalid).is_err());
    }
}
