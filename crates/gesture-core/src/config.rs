use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::Path,
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

fn default_version() -> u32 {
    1
}

fn default_log_level() -> String {
    "info".to_owned()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            runtime: RuntimeConfig::default(),
            security: SecurityConfig::default(),
            bindings: Vec::new(),
        }
    }
}

impl Config {
    pub fn parse(source: &str) -> Result<Self> {
        let config: Self = toml::from_str(source).context("failed to parse TOML configuration")?;
        config.validate()?;
        Ok(config)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let source = fs::read_to_string(path)
            .with_context(|| format!("failed to read configuration {}", path.display()))?;
        Self::parse(&source).with_context(|| format!("invalid configuration {}", path.display()))
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!(
                "unsupported configuration version {}; expected 1",
                self.version
            );
        }

        let mut ids = HashSet::new();
        for binding in &self.bindings {
            binding.validate()?;
            if !ids.insert(binding.id.as_str()) {
                bail!("duplicate binding id {:?}", binding.id);
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub socket_path: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            socket_path: String::new(),
            log_level: default_log_level(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default)]
    pub allow_command_actions: bool,
    #[serde(default)]
    pub allow_uinput_actions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub consume: bool,
    pub trigger: TriggerPattern,
    #[serde(default)]
    pub conditions: Vec<ConditionSpec>,
    #[serde(default)]
    pub actions: Vec<ActionSpec>,
}

impl Binding {
    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            bail!("binding id must not be empty");
        }
        self.trigger
            .validate()
            .with_context(|| format!("binding {:?} has an invalid trigger", self.id))?;
        if self.actions.is_empty() {
            bail!("binding {:?} must define at least one action", self.id);
        }
        for action in &self.actions {
            action
                .validate()
                .with_context(|| format!("binding {:?} has an invalid action", self.id))?;
        }
        for condition in &self.conditions {
            condition
                .validate()
                .with_context(|| format!("binding {:?} has an invalid condition", self.id))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerPattern {
    pub family: String,
    #[serde(default)]
    pub phases: Vec<String>,
    #[serde(default)]
    pub fingers: Vec<u8>,
    #[serde(default)]
    pub directions: Vec<String>,
    #[serde(default)]
    pub min_values: BTreeMap<String, f64>,
    #[serde(default)]
    pub max_values: BTreeMap<String, f64>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

impl TriggerPattern {
    pub fn validate(&self) -> Result<()> {
        if self.family.trim().is_empty() {
            bail!("trigger family must not be empty");
        }
        for (name, min) in &self.min_values {
            if let Some(max) = self.max_values.get(name) {
                if min > max {
                    bail!("minimum value for {name:?} exceeds maximum value");
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionSpec {
    pub provider: String,
    pub condition: String,
    #[serde(default)]
    pub negate: bool,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl ConditionSpec {
    pub fn validate(&self) -> Result<()> {
        validate_identifier("condition provider", &self.provider)?;
        validate_identifier("condition name", &self.condition)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSpec {
    pub provider: String,
    pub action: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default)]
    pub on_error: ErrorPolicy,
}

impl ActionSpec {
    pub fn validate(&self) -> Result<()> {
        validate_identifier("action provider", &self.provider)?;
        validate_identifier("action name", &self.action)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorPolicy {
    #[default]
    Continue,
    StopBinding,
    StopDispatch,
}

fn validate_identifier(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{label} must not be empty");
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        bail!("{label} contains unsupported characters: {value:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn example_configuration_parses() {
        let source = include_str!("../../../configs/config.example.toml");
        let config = Config::parse(source).expect("example configuration should be valid");
        assert_eq!(config.version, 1);
        assert_eq!(config.bindings.len(), 1);
    }

    #[test]
    fn duplicate_binding_ids_are_rejected() {
        let source = r#"
version = 1

[[bindings]]
id = "same"
[bindings.trigger]
family = "test.event"
[[bindings.actions]]
provider = "core"
action = "noop"

[[bindings]]
id = "same"
[bindings.trigger]
family = "test.event"
[[bindings.actions]]
provider = "core"
action = "noop"
"#;
        assert!(Config::parse(source).is_err());
    }
}
