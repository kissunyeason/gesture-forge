use std::{thread, time::Duration};

use anyhow::{bail, Context, Result};
use evdev::{
    uinput::VirtualDevice, AttributeSet, EventType, InputEvent as EvdevInputEvent, KeyCode,
};
use gesture_core::ActionSpec;
use serde::Deserialize;

const DEVICE_NAME: &str = "GestureForge Virtual Keyboard";
const KEYBOARD_CODE_LIMIT_EXCLUSIVE: u16 = 0x100;
const MAX_CHORD_KEYS: usize = 8;
const DEFAULT_HOLD_MS: u64 = 50;
const MAX_HOLD_MS: u64 = 1_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyChordParams {
    keys: Vec<String>,
    #[serde(default = "default_hold_ms")]
    hold_ms: u64,
}

fn default_hold_ms() -> u64 {
    DEFAULT_HOLD_MS
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyChord {
    pub(crate) keys: Vec<KeyCode>,
    pub(crate) hold: Duration,
}

pub(crate) fn parse_key_chord(spec: &ActionSpec) -> Result<KeyChord> {
    let params: KeyChordParams = serde_json::from_value(spec.params.clone())
        .context("uinput.key-chord expects { keys = [\"KEY_LEFTMETA\", ...], hold_ms? }")?;
    if params.keys.is_empty() {
        bail!("uinput.key-chord keys must not be empty");
    }
    if params.hold_ms == 0 || params.hold_ms > MAX_HOLD_MS {
        bail!("uinput.key-chord hold_ms must be between 1 and {MAX_HOLD_MS}");
    }
    if params.keys.len() > MAX_CHORD_KEYS {
        bail!("uinput.key-chord supports at most {MAX_CHORD_KEYS} simultaneous keys");
    }

    let mut parsed = Vec::with_capacity(params.keys.len());
    for raw_name in params.keys {
        let name = raw_name.trim().to_ascii_uppercase();
        if name.is_empty() {
            bail!("uinput.key-chord key names must not be empty");
        }
        let key = name
            .parse::<KeyCode>()
            .map_err(|error| anyhow::anyhow!("unsupported uinput key name {name:?}: {error}"))?;
        if key.code() == 0 || key.code() >= KEYBOARD_CODE_LIMIT_EXCLUSIVE {
            bail!("uinput.key-chord key {name:?} is not a standard keyboard key");
        }
        if parsed.contains(&key) {
            bail!("uinput.key-chord contains duplicate key {name:?}");
        }
        parsed.push(key);
    }
    Ok(KeyChord {
        keys: parsed,
        hold: Duration::from_millis(params.hold_ms),
    })
}

#[derive(Default)]
pub(crate) struct UinputKeyboardRuntime {
    device: Option<VirtualDevice>,
    pressed: Vec<KeyCode>,
}

impl UinputKeyboardRuntime {
    pub(crate) fn tap_chord(&mut self, keys: &[KeyCode], hold: Duration) -> Result<()> {
        debug_assert!(!keys.is_empty());
        self.release_all_keys()
            .context("failed to clear stale virtual keyboard state")?;

        self.device()?;
        self.pressed = keys.to_vec();
        let press_result = self
            .emit_key_frames(keys.iter().copied(), 1)
            .context("failed to press virtual key chord");
        if let Err(error) = press_result {
            let release_result = self.release_all_keys();
            if let Err(release_error) = release_result {
                return Err(error).context(format!(
                    "failed to press virtual key chord; emergency release also failed: {release_error}"
                ));
            }
            return Err(error)
                .context("failed to press virtual key chord; emergency release was attempted");
        }

        thread::sleep(hold);

        let release_result = self
            .emit_key_frames(keys.iter().rev().copied(), 0)
            .context("failed to release virtual key chord");
        if let Err(error) = release_result {
            let release_result = self.release_all_keys();
            if let Err(release_error) = release_result {
                return Err(error).context(format!(
                    "failed to release virtual key chord; emergency release also failed: {release_error}"
                ));
            }
            return Err(error)
                .context("failed to release virtual key chord; emergency release was attempted");
        }

        self.pressed.clear();
        Ok(())
    }

    pub(crate) fn release_all_keys(&mut self) -> Result<()> {
        if self.pressed.is_empty() {
            return Ok(());
        }

        let keys = self.pressed.clone();
        let result = if self.device.is_some() {
            self.emit_key_frames(keys.into_iter().rev(), 0)
                .context("failed to release virtual keyboard keys")
        } else {
            Ok(())
        };
        self.pressed.clear();
        result
    }

    fn emit_key_frames(
        &mut self,
        keys: impl IntoIterator<Item = KeyCode>,
        value: i32,
    ) -> Result<()> {
        for events in key_event_packets(keys, value) {
            self.device()?
                .emit(&events)
                .context("failed to emit virtual keyboard key frame")?;
        }
        Ok(())
    }

    fn device(&mut self) -> Result<&mut VirtualDevice> {
        if self.device.is_none() {
            self.device = Some(create_virtual_keyboard()?);
        }
        Ok(self.device.as_mut().expect("device initialized above"))
    }
}

fn create_virtual_keyboard() -> Result<VirtualDevice> {
    let keys = AttributeSet::from_iter((1u16..KEYBOARD_CODE_LIMIT_EXCLUSIVE).map(KeyCode));
    VirtualDevice::builder()
        .context("failed to open /dev/uinput")?
        .name(DEVICE_NAME)
        .with_keys(&keys)
        .context("failed to configure virtual keyboard keys")?
        .build()
        .context("failed to create virtual keyboard")
}

fn key_event_packets(
    keys: impl IntoIterator<Item = KeyCode>,
    value: i32,
) -> Vec<Vec<EvdevInputEvent>> {
    keys.into_iter()
        .map(|key| {
            vec![EvdevInputEvent::new_now(
                EventType::KEY.0,
                key.code(),
                value,
            )]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gesture_core::ErrorPolicy;

    fn spec(keys: serde_json::Value) -> ActionSpec {
        ActionSpec {
            provider: "uinput".to_owned(),
            action: "key-chord".to_owned(),
            params: serde_json::json!({ "keys": keys }),
            on_error: ErrorPolicy::Continue,
        }
    }

    #[test]
    fn parses_workspace_shortcuts_without_opening_uinput() {
        let chord =
            parse_key_chord(&spec(serde_json::json!(["key_leftmeta", " KEY_PAGEDOWN "]))).unwrap();
        assert_eq!(
            chord.keys,
            vec![KeyCode::KEY_LEFTMETA, KeyCode::KEY_PAGEDOWN]
        );
        assert_eq!(chord.hold, Duration::from_millis(DEFAULT_HOLD_MS));
    }

    #[test]
    fn rejects_empty_duplicate_and_button_chords() {
        assert!(parse_key_chord(&spec(serde_json::json!([]))).is_err());
        assert!(
            parse_key_chord(&spec(serde_json::json!(["KEY_LEFTMETA", "KEY_LEFTMETA"]))).is_err()
        );
        assert!(parse_key_chord(&spec(serde_json::json!(["BTN_LEFT"]))).is_err());
    }

    #[test]
    fn presses_in_order_and_releases_in_reverse_order_as_separate_frames() {
        let keys = [KeyCode::KEY_LEFTMETA, KeyCode::KEY_PAGEUP];
        let down = key_event_packets(keys, 1);
        let up = key_event_packets(keys.into_iter().rev(), 0);

        assert_eq!(down.len(), 2);
        assert!(down.iter().all(|packet| packet.len() == 1));
        assert_eq!(down[0][0].code(), KeyCode::KEY_LEFTMETA.code());
        assert_eq!(down[1][0].code(), KeyCode::KEY_PAGEUP.code());
        assert!(down.iter().flatten().all(|event| event.value() == 1));
        assert_eq!(up[0][0].code(), KeyCode::KEY_PAGEUP.code());
        assert_eq!(up[1][0].code(), KeyCode::KEY_LEFTMETA.code());
        assert!(up.iter().flatten().all(|event| event.value() == 0));
    }

    #[test]
    fn parses_and_validates_hold_duration() {
        let mut spec = spec(serde_json::json!(["KEY_LEFTMETA", "KEY_PAGEDOWN"]));
        spec.params["hold_ms"] = serde_json::json!(80);
        assert_eq!(
            parse_key_chord(&spec).unwrap().hold,
            Duration::from_millis(80)
        );

        spec.params["hold_ms"] = serde_json::json!(0);
        assert!(parse_key_chord(&spec).is_err());
        spec.params["hold_ms"] = serde_json::json!(MAX_HOLD_MS + 1);
        assert!(parse_key_chord(&spec).is_err());
    }
}
