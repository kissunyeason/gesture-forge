//! Built-in action and condition providers.

mod uinput_keyboard;
mod uinput_pointer;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use gesture_core::{
    ActionOutcome, ActionProvider, ActionRegistry, ActionSpec, ConditionProvider,
    ConditionRegistry, ConditionSpec, InputEvent,
};
use serde::Deserialize;
use tokio::process::Command;
use tracing::{debug, error, info, warn};

pub use uinput_pointer::UinputPointerProvider;

pub fn default_action_registry(allow_commands: bool) -> Result<ActionRegistry> {
    default_action_registry_with_security(allow_commands, false)
}

pub fn default_action_registry_with_security(
    allow_commands: bool,
    allow_uinput: bool,
) -> Result<ActionRegistry> {
    let mut registry = ActionRegistry::default();
    registry.register(CoreActionProvider)?;
    if allow_commands {
        registry.register(ProcessActionProvider)?;
    }
    if allow_uinput {
        registry.register(UinputPointerProvider::new())?;
    }
    Ok(registry)
}

pub fn default_condition_registry() -> Result<ConditionRegistry> {
    let mut registry = ConditionRegistry::default();
    registry.register(CoreConditionProvider)?;
    Ok(registry)
}

pub struct CoreActionProvider;

#[derive(Debug, Deserialize)]
struct LogParams {
    message: String,
    #[serde(default = "default_info_level")]
    level: String,
}

fn default_info_level() -> String {
    "info".to_owned()
}

#[async_trait]
impl ActionProvider for CoreActionProvider {
    fn name(&self) -> &'static str {
        "core"
    }

    fn validate(&self, spec: &ActionSpec) -> Result<()> {
        match spec.action.as_str() {
            "noop" => Ok(()),
            "log" => {
                let params: LogParams = serde_json::from_value(spec.params.clone())
                    .context("core.log expects { message, level? }")?;
                if params.message.trim().is_empty() {
                    bail!("core.log message must not be empty");
                }
                match params.level.as_str() {
                    "debug" | "info" | "warn" | "error" => Ok(()),
                    other => bail!("unsupported core.log level {other:?}"),
                }
            }
            other => bail!("unknown core action {other:?}"),
        }
    }

    async fn execute(&self, spec: &ActionSpec, event: &InputEvent) -> Result<ActionOutcome> {
        self.validate(spec)?;
        match spec.action.as_str() {
            "noop" => Ok(ActionOutcome::success(spec, Some("no operation".into()))),
            "log" => {
                let params: LogParams = serde_json::from_value(spec.params.clone())?;
                match params.level.as_str() {
                    "debug" => debug!(event_id = %event.id, "{}", params.message),
                    "info" => info!(event_id = %event.id, "{}", params.message),
                    "warn" => warn!(event_id = %event.id, "{}", params.message),
                    "error" => error!(event_id = %event.id, "{}", params.message),
                    _ => unreachable!("validated above"),
                }
                Ok(ActionOutcome::success(spec, Some(params.message)))
            }
            _ => unreachable!("validated above"),
        }
    }
}

pub struct ProcessActionProvider;

#[derive(Debug, Deserialize)]
struct RunParams {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    wait: bool,
}

#[async_trait]
impl ActionProvider for ProcessActionProvider {
    fn name(&self) -> &'static str {
        "process"
    }

    fn validate(&self, spec: &ActionSpec) -> Result<()> {
        if spec.action != "run" {
            bail!("unknown process action {:?}", spec.action);
        }
        let params: RunParams = serde_json::from_value(spec.params.clone())
            .context("process.run expects { program, args?, wait? }")?;
        if params.program.trim().is_empty() {
            bail!("process.run program must not be empty");
        }
        Ok(())
    }

    async fn execute(&self, spec: &ActionSpec, _event: &InputEvent) -> Result<ActionOutcome> {
        self.validate(spec)?;
        let params: RunParams = serde_json::from_value(spec.params.clone())?;
        let mut command = Command::new(&params.program);
        command.args(&params.args);

        if params.wait {
            let status = command
                .status()
                .await
                .with_context(|| format!("failed to launch {:?}", params.program))?;
            if !status.success() {
                bail!("program {:?} exited with {status}", params.program);
            }
            Ok(ActionOutcome::success(
                spec,
                Some(format!("program exited successfully: {}", params.program)),
            ))
        } else {
            command
                .spawn()
                .with_context(|| format!("failed to launch {:?}", params.program))?;
            Ok(ActionOutcome::success(
                spec,
                Some(format!("program started: {}", params.program)),
            ))
        }
    }
}

pub struct CoreConditionProvider;

#[async_trait]
impl ConditionProvider for CoreConditionProvider {
    fn name(&self) -> &'static str {
        "core"
    }

    fn validate(&self, spec: &ConditionSpec) -> Result<()> {
        match spec.condition.as_str() {
            "always" => Ok(()),
            "app-id" | "window-title" | "label" | "context" => {
                let object = spec
                    .params
                    .as_object()
                    .context("condition parameters must be a table")?;
                if !object.contains_key("value") {
                    bail!("condition requires a value parameter");
                }
                if (spec.condition == "label" || spec.condition == "context")
                    && !object.get("key").is_some_and(|value| value.is_string())
                {
                    bail!("label/context condition requires a string key parameter");
                }
                Ok(())
            }
            other => bail!("unknown core condition {other:?}"),
        }
    }

    async fn evaluate(&self, spec: &ConditionSpec, event: &InputEvent) -> Result<bool> {
        self.validate(spec)?;
        match spec.condition.as_str() {
            "always" => Ok(true),
            "app-id" => Ok(event.context.app_id.as_deref() == spec.params["value"].as_str()),
            "window-title" => {
                Ok(event.context.window_title.as_deref() == spec.params["value"].as_str())
            }
            "label" => {
                let key = spec.params["key"]
                    .as_str()
                    .context("label key must be a string")?;
                Ok(event.labels.get(key).map(String::as_str) == spec.params["value"].as_str())
            }
            "context" => {
                let key = spec.params["key"]
                    .as_str()
                    .context("context key must be a string")?;
                Ok(event.context.extra.get(key) == spec.params.get("value"))
            }
            _ => unreachable!("validated above"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gesture_core::{ActionSpec, ErrorPolicy};

    fn uinput_drag_spec() -> ActionSpec {
        ActionSpec {
            provider: "uinput".to_owned(),
            action: "drag".to_owned(),
            params: serde_json::json!({}),
            on_error: ErrorPolicy::Continue,
        }
    }

    fn uinput_key_chord_spec() -> ActionSpec {
        ActionSpec {
            provider: "uinput".to_owned(),
            action: "key-chord".to_owned(),
            params: serde_json::json!({
                "keys": ["KEY_LEFTMETA", "KEY_PAGEUP"]
            }),
            on_error: ErrorPolicy::Continue,
        }
    }

    #[test]
    fn uinput_provider_requires_explicit_registry_opt_in() {
        let disabled = default_action_registry_with_security(false, false).unwrap();
        assert!(disabled.validate(&uinput_drag_spec()).is_err());
        assert!(disabled.validate(&uinput_key_chord_spec()).is_err());

        let enabled = default_action_registry_with_security(false, true).unwrap();
        enabled.validate(&uinput_drag_spec()).unwrap();
        enabled.validate(&uinput_key_chord_spec()).unwrap();
    }
}
