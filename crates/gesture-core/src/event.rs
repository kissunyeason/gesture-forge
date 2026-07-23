use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn default_schema_version() -> u32 {
    1
}

/// A normalized event published by any input backend or recognizer.
///
/// Namespaced strings deliberately avoid coupling the core to a fixed list of
/// gestures. New backends can add event families without changing this crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputEvent {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub family: String,
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingers: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(default)]
    pub values: BTreeMap<String, f64>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub context: EventContext,
}

impl InputEvent {
    pub fn new(family: impl Into<String>, phase: impl Into<String>) -> Self {
        Self {
            schema_version: 1,
            id: Uuid::new_v4(),
            family: family.into(),
            phase: phase.into(),
            fingers: None,
            direction: None,
            values: BTreeMap::new(),
            labels: BTreeMap::new(),
            context: EventContext::default(),
        }
    }
}

/// Desktop and session data attached by optional context adapters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    #[serde(default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
