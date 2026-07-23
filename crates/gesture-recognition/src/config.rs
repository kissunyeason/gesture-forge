use std::{collections::BTreeSet, fs, path::Path};

use anyhow::{bail, Context, Result};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecognizerConfig {
    pub recognition: RecognitionSettings,
}

impl RecognizerConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read recognizer config {}", path.display()))?;
        let config: Self = toml::from_str(&text)
            .with_context(|| format!("failed to parse recognizer config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let mut ids = BTreeSet::new();

        for rule in &self.recognition.swipes {
            rule.validate()?;
            if !ids.insert(rule.id.as_str()) {
                bail!("duplicate recognition rule id {:?}", rule.id);
            }
        }
        for rule in &self.recognition.holds {
            rule.validate()?;
            if !ids.insert(rule.id.as_str()) {
                bail!("duplicate recognition rule id {:?}", rule.id);
            }
        }
        let mut drag_finger_counts = BTreeSet::new();
        for rule in &self.recognition.drags {
            rule.validate()?;
            if !ids.insert(rule.id.as_str()) {
                bail!("duplicate recognition rule id {:?}", rule.id);
            }
            if rule.enabled && !drag_finger_counts.insert(rule.fingers) {
                bail!(
                    "multiple enabled drag rules use {} fingers; continuous rule selection would be ambiguous",
                    rule.fingers
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RecognitionSettings {
    pub swipes: Vec<SwipeRuleConfig>,
    pub holds: Vec<HoldRuleConfig>,
    pub drags: Vec<DragRuleConfig>,
}

impl Default for RecognitionSettings {
    fn default() -> Self {
        Self {
            swipes: vec![SwipeRuleConfig::default()],
            holds: vec![HoldRuleConfig::default()],
            drags: Vec::new(),
        }
    }
}

impl<'de> Deserialize<'de> for RecognitionSettings {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RecognitionSettingsWire::deserialize(deserializer)?;

        if wire.swipes.is_some() && wire.three_finger_swipe.is_some() {
            return Err(D::Error::custom(
                "recognition.swipes cannot be combined with legacy recognition.three_finger_swipe",
            ));
        }
        if wire.holds.is_some() && wire.three_finger_hold.is_some() {
            return Err(D::Error::custom(
                "recognition.holds cannot be combined with legacy recognition.three_finger_hold",
            ));
        }

        let swipes = match (wire.swipes, wire.three_finger_swipe) {
            (Some(rules), None) => rules,
            (None, Some(rule)) => vec![rule.into_rule()],
            (None, None) => vec![SwipeRuleConfig::default()],
            (Some(_), Some(_)) => unreachable!("conflicts handled above"),
        };
        let holds = match (wire.holds, wire.three_finger_hold) {
            (Some(rules), None) => rules,
            (None, Some(rule)) => vec![rule.into_rule()],
            (None, None) => vec![HoldRuleConfig::default()],
            (Some(_), Some(_)) => unreachable!("conflicts handled above"),
        };

        Ok(Self {
            swipes,
            holds,
            drags: wire.drags.unwrap_or_default(),
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RecognitionSettingsWire {
    swipes: Option<Vec<SwipeRuleConfig>>,
    holds: Option<Vec<HoldRuleConfig>>,
    drags: Option<Vec<DragRuleConfig>>,
    three_finger_swipe: Option<LegacySwipeRecognizerConfig>,
    three_finger_hold: Option<LegacyHoldRecognizerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwipeRuleConfig {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub fingers: u8,
    pub min_distance: f64,
    pub min_average_velocity: f64,
    pub max_duration_ms: f64,
    pub max_axis_deviation_degrees: f64,
    pub require_complete_tracking: bool,
}

impl Default for SwipeRuleConfig {
    fn default() -> Self {
        Self {
            id: "three-finger-swipe".to_owned(),
            enabled: true,
            fingers: 3,
            min_distance: 200.0,
            min_average_velocity: 400.0,
            max_duration_ms: 900.0,
            max_axis_deviation_degrees: 30.0,
            require_complete_tracking: true,
        }
    }
}

impl SwipeRuleConfig {
    fn validate(&self) -> Result<()> {
        validate_rule_id(&self.id)?;
        if self.fingers == 0 {
            bail!(
                "swipe rule {:?} finger count must be greater than zero",
                self.id
            );
        }
        if !self.min_distance.is_finite() || self.min_distance < 0.0 {
            bail!(
                "swipe rule {:?} min_distance must be a finite non-negative number",
                self.id
            );
        }
        if !self.min_average_velocity.is_finite() || self.min_average_velocity < 0.0 {
            bail!(
                "swipe rule {:?} min_average_velocity must be a finite non-negative number",
                self.id
            );
        }
        if !self.max_duration_ms.is_finite() || self.max_duration_ms <= 0.0 {
            bail!(
                "swipe rule {:?} max_duration_ms must be a finite positive number",
                self.id
            );
        }
        if !self.max_axis_deviation_degrees.is_finite()
            || !(0.0..=45.0).contains(&self.max_axis_deviation_degrees)
        {
            bail!(
                "swipe rule {:?} max_axis_deviation_degrees must be between 0 and 45",
                self.id
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldRuleConfig {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub fingers: u8,
    pub min_duration_ms: f64,
    pub max_net_distance: f64,
    pub require_complete_tracking: bool,
}

impl Default for HoldRuleConfig {
    fn default() -> Self {
        Self {
            id: "three-finger-hold".to_owned(),
            enabled: true,
            fingers: 3,
            min_duration_ms: 650.0,
            max_net_distance: 30.0,
            require_complete_tracking: false,
        }
    }
}

impl HoldRuleConfig {
    fn validate(&self) -> Result<()> {
        validate_rule_id(&self.id)?;
        if self.fingers == 0 {
            bail!(
                "hold rule {:?} finger count must be greater than zero",
                self.id
            );
        }
        if !self.min_duration_ms.is_finite() || self.min_duration_ms <= 0.0 {
            bail!(
                "hold rule {:?} min_duration_ms must be a finite positive number",
                self.id
            );
        }
        if !self.max_net_distance.is_finite() || self.max_net_distance < 0.0 {
            bail!(
                "hold rule {:?} max_net_distance must be a finite non-negative number",
                self.id
            );
        }
        Ok(())
    }
}

/// Compatibility name retained for Rust callers migrating from v0.4.
pub type SwipeRecognizerConfig = SwipeRuleConfig;
/// Compatibility name retained for Rust callers migrating from v0.4.
pub type HoldRecognizerConfig = HoldRuleConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DragRuleConfig {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub fingers: u8,
    pub min_hold_duration_ms: f64,
    pub max_hold_distance: f64,
    pub min_drag_distance: f64,
    pub require_complete_tracking: bool,
}

impl DragRuleConfig {
    fn validate(&self) -> Result<()> {
        validate_rule_id(&self.id)?;
        if self.fingers == 0 {
            bail!(
                "drag rule {:?} finger count must be greater than zero",
                self.id
            );
        }
        if !self.min_hold_duration_ms.is_finite() || self.min_hold_duration_ms <= 0.0 {
            bail!(
                "drag rule {:?} min_hold_duration_ms must be a finite positive number",
                self.id
            );
        }
        if !self.max_hold_distance.is_finite() || self.max_hold_distance < 0.0 {
            bail!(
                "drag rule {:?} max_hold_distance must be a finite non-negative number",
                self.id
            );
        }
        if !self.min_drag_distance.is_finite() || self.min_drag_distance < 0.0 {
            bail!(
                "drag rule {:?} min_drag_distance must be a finite non-negative number",
                self.id
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LegacySwipeRecognizerConfig {
    enabled: bool,
    fingers: u8,
    min_distance: f64,
    min_average_velocity: f64,
    max_duration_ms: f64,
    max_axis_deviation_degrees: f64,
}

impl Default for LegacySwipeRecognizerConfig {
    fn default() -> Self {
        let rule = SwipeRuleConfig::default();
        Self {
            enabled: rule.enabled,
            fingers: rule.fingers,
            min_distance: rule.min_distance,
            min_average_velocity: rule.min_average_velocity,
            max_duration_ms: rule.max_duration_ms,
            max_axis_deviation_degrees: rule.max_axis_deviation_degrees,
        }
    }
}

impl LegacySwipeRecognizerConfig {
    fn into_rule(self) -> SwipeRuleConfig {
        SwipeRuleConfig {
            id: "three-finger-swipe".to_owned(),
            enabled: self.enabled,
            fingers: self.fingers,
            min_distance: self.min_distance,
            min_average_velocity: self.min_average_velocity,
            max_duration_ms: self.max_duration_ms,
            max_axis_deviation_degrees: self.max_axis_deviation_degrees,
            // v0.4 accepted reported fingers with partial coordinate tracking.
            require_complete_tracking: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LegacyHoldRecognizerConfig {
    enabled: bool,
    fingers: u8,
    min_duration_ms: f64,
    max_net_distance: f64,
}

impl Default for LegacyHoldRecognizerConfig {
    fn default() -> Self {
        let rule = HoldRuleConfig::default();
        Self {
            enabled: rule.enabled,
            fingers: rule.fingers,
            min_duration_ms: rule.min_duration_ms,
            max_net_distance: rule.max_net_distance,
        }
    }
}

impl LegacyHoldRecognizerConfig {
    fn into_rule(self) -> HoldRuleConfig {
        HoldRuleConfig {
            id: "three-finger-hold".to_owned(),
            enabled: self.enabled,
            fingers: self.fingers,
            min_duration_ms: self.min_duration_ms,
            max_net_distance: self.max_net_distance,
            require_complete_tracking: false,
        }
    }
}

fn default_true() -> bool {
    true
}

fn validate_rule_id(id: &str) -> Result<()> {
    if id.trim().is_empty() {
        bail!("recognition rule id must not be empty");
    }
    if !id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        bail!("recognition rule id contains unsupported characters: {id:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_partial_configuration_with_defaults() {
        let config: RecognizerConfig = toml::from_str(
            r#"
                [recognition.three_finger_swipe]
                min_distance = 250.0
            "#,
        )
        .unwrap();

        assert_eq!(config.recognition.swipes[0].min_distance, 250.0);
        assert_eq!(config.recognition.swipes[0].min_average_velocity, 400.0);
        assert!(!config.recognition.swipes[0].require_complete_tracking);
        assert_eq!(config.recognition.holds[0].fingers, 3);
    }

    #[test]
    fn parses_multiple_generic_rules() {
        let config: RecognizerConfig = toml::from_str(
            r#"
                [recognition]
                holds = []

                [[recognition.swipes]]
                id = "three"
                fingers = 3
                min_distance = 200.0
                min_average_velocity = 400.0
                max_duration_ms = 900.0
                max_axis_deviation_degrees = 30.0
                require_complete_tracking = true

                [[recognition.swipes]]
                id = "four"
                fingers = 4
                min_distance = 240.0
                min_average_velocity = 450.0
                max_duration_ms = 900.0
                max_axis_deviation_degrees = 30.0
                require_complete_tracking = true
            "#,
        )
        .unwrap();

        assert_eq!(config.recognition.swipes.len(), 2);
        assert_eq!(config.recognition.swipes[1].fingers, 4);
        assert!(config.recognition.holds.is_empty());
        assert!(config.recognition.drags.is_empty());
        config.validate().unwrap();
    }

    #[test]
    fn rejects_legacy_and_generic_swipes_together() {
        let result = toml::from_str::<RecognizerConfig>(
            r#"
                [recognition.three_finger_swipe]
                enabled = true

                [[recognition.swipes]]
                id = "three"
                fingers = 3
                min_distance = 200.0
                min_average_velocity = 400.0
                max_duration_ms = 900.0
                max_axis_deviation_degrees = 30.0
                require_complete_tracking = true
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_duplicate_rule_ids() {
        let mut config = RecognizerConfig::default();
        config.recognition.holds[0].id = config.recognition.swipes[0].id.clone();
        assert!(config.validate().is_err());
    }

    #[test]
    fn explicit_empty_lists_disable_all_rules() {
        let config: RecognizerConfig = toml::from_str(
            r#"
                [recognition]
                swipes = []
                holds = []
                drags = []
            "#,
        )
        .unwrap();

        assert!(config.recognition.swipes.is_empty());
        assert!(config.recognition.holds.is_empty());
        assert!(config.recognition.drags.is_empty());
        config.validate().unwrap();
    }

    #[test]
    fn rejects_zero_finger_rules() {
        let mut config = RecognizerConfig::default();
        config.recognition.swipes[0].fingers = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn parses_drag_rule_and_rejects_ambiguous_finger_count() {
        let mut config: RecognizerConfig = toml::from_str(
            r#"
                [[recognition.drags]]
                id = "three-finger-drag"
                fingers = 3
                min_hold_duration_ms = 350.0
                max_hold_distance = 20.0
                min_drag_distance = 8.0
                require_complete_tracking = true
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.recognition.drags[0].fingers, 3);

        config.recognition.drags.push(DragRuleConfig {
            id: "other-three-finger-drag".to_owned(),
            enabled: true,
            fingers: 3,
            min_hold_duration_ms: 400.0,
            max_hold_distance: 20.0,
            min_drag_distance: 8.0,
            require_complete_tracking: true,
        });
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_direction_tolerance() {
        let mut config = RecognizerConfig::default();
        config.recognition.swipes[0].max_axis_deviation_degrees = 50.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        let result = toml::from_str::<RecognizerConfig>(
            r#"
                [[recognition.holds]]
                id = "four"
                fingers = 4
                min_duration_ms = 700.0
                max_net_distance = 30.0
                require_complete_tracking = true
                misspelled_option = true
            "#,
        );
        assert!(result.is_err());
    }
}
