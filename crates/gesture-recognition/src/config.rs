use std::{fs, path::Path};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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
        self.recognition.three_finger_swipe.validate()?;
        self.recognition.three_finger_hold.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RecognitionSettings {
    pub three_finger_swipe: SwipeRecognizerConfig,
    pub three_finger_hold: HoldRecognizerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SwipeRecognizerConfig {
    pub enabled: bool,
    pub fingers: u8,
    pub min_distance: f64,
    pub min_average_velocity: f64,
    pub max_duration_ms: f64,
    pub max_axis_deviation_degrees: f64,
}

impl Default for SwipeRecognizerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fingers: 3,
            min_distance: 200.0,
            min_average_velocity: 400.0,
            max_duration_ms: 900.0,
            max_axis_deviation_degrees: 30.0,
        }
    }
}

impl SwipeRecognizerConfig {
    fn validate(&self) -> Result<()> {
        if self.fingers == 0 {
            bail!("swipe finger count must be greater than zero");
        }
        if !self.min_distance.is_finite() || self.min_distance < 0.0 {
            bail!("swipe min_distance must be a finite non-negative number");
        }
        if !self.min_average_velocity.is_finite() || self.min_average_velocity < 0.0 {
            bail!("swipe min_average_velocity must be a finite non-negative number");
        }
        if !self.max_duration_ms.is_finite() || self.max_duration_ms <= 0.0 {
            bail!("swipe max_duration_ms must be a finite positive number");
        }
        if !self.max_axis_deviation_degrees.is_finite()
            || !(0.0..=45.0).contains(&self.max_axis_deviation_degrees)
        {
            bail!("swipe max_axis_deviation_degrees must be between 0 and 45");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HoldRecognizerConfig {
    pub enabled: bool,
    pub fingers: u8,
    pub min_duration_ms: f64,
    pub max_net_distance: f64,
}

impl Default for HoldRecognizerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fingers: 3,
            min_duration_ms: 650.0,
            max_net_distance: 30.0,
        }
    }
}

impl HoldRecognizerConfig {
    fn validate(&self) -> Result<()> {
        if self.fingers == 0 {
            bail!("hold finger count must be greater than zero");
        }
        if !self.min_duration_ms.is_finite() || self.min_duration_ms <= 0.0 {
            bail!("hold min_duration_ms must be a finite positive number");
        }
        if !self.max_net_distance.is_finite() || self.max_net_distance < 0.0 {
            bail!("hold max_net_distance must be a finite non-negative number");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_configuration_with_defaults() {
        let config: RecognizerConfig = toml::from_str(
            r#"
                [recognition.three_finger_swipe]
                min_distance = 250.0
            "#,
        )
        .unwrap();

        assert_eq!(config.recognition.three_finger_swipe.min_distance, 250.0);
        assert_eq!(
            config.recognition.three_finger_swipe.min_average_velocity,
            400.0
        );
        assert_eq!(config.recognition.three_finger_hold.fingers, 3);
    }

    #[test]
    fn rejects_invalid_direction_tolerance() {
        let mut config = RecognizerConfig::default();
        config
            .recognition
            .three_finger_swipe
            .max_axis_deviation_degrees = 50.0;
        assert!(config.validate().is_err());
    }
}
