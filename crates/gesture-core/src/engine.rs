use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::{
    ActionRegistry, ActionSpec, Binding, ConditionRegistry, Config, ErrorPolicy, InputEvent,
    TriggerPattern,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchPlan {
    pub event_id: uuid::Uuid,
    pub bindings: Vec<PlannedBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedBinding {
    pub id: String,
    pub priority: i32,
    pub consume: bool,
    pub actions: Vec<ActionSpec>,
}

#[derive(Clone)]
pub struct Engine {
    config: Config,
}

impl Engine {
    pub fn new(config: Config) -> Result<Self> {
        config.validate()?;
        Ok(Self { config })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn validate_providers(
        &self,
        actions: &ActionRegistry,
        conditions: &ConditionRegistry,
    ) -> Result<()> {
        for binding in &self.config.bindings {
            for action in &binding.actions {
                actions.validate(action)?;
            }
            for condition in &binding.conditions {
                conditions.validate(condition)?;
            }
        }
        Ok(())
    }

    pub async fn plan(
        &self,
        event: &InputEvent,
        conditions: &ConditionRegistry,
    ) -> Result<DispatchPlan> {
        if event.schema_version != 1 {
            bail!(
                "unsupported event schema version {}; expected 1",
                event.schema_version
            );
        }

        let mut bindings: Vec<&Binding> = self
            .config
            .bindings
            .iter()
            .filter(|binding| binding.enabled && trigger_matches(&binding.trigger, event))
            .collect();

        bindings.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut planned = Vec::new();
        for binding in bindings {
            let mut conditions_match = true;
            for condition in &binding.conditions {
                if !conditions.evaluate(condition, event).await? {
                    conditions_match = false;
                    break;
                }
            }

            if conditions_match {
                planned.push(PlannedBinding {
                    id: binding.id.clone(),
                    priority: binding.priority,
                    consume: binding.consume,
                    actions: binding.actions.clone(),
                });
                if binding.consume {
                    break;
                }
            }
        }

        Ok(DispatchPlan {
            event_id: event.id,
            bindings: planned,
        })
    }

    pub async fn dispatch(
        &self,
        event: &InputEvent,
        actions: &ActionRegistry,
        conditions: &ConditionRegistry,
    ) -> Result<DispatchReport> {
        let plan = self.plan(event, conditions).await?;
        let mut report = DispatchReport {
            event_id: event.id,
            bindings: Vec::new(),
        };

        'dispatch: for binding in plan.bindings {
            let mut binding_report = BindingReport {
                id: binding.id,
                outcomes: Vec::new(),
            };

            for action in binding.actions {
                match actions.execute(&action, event).await {
                    Ok(outcome) => binding_report.outcomes.push(outcome),
                    Err(error) => {
                        binding_report.outcomes.push(crate::ActionOutcome {
                            provider: action.provider.clone(),
                            action: action.action.clone(),
                            success: false,
                            message: Some(error.to_string()),
                        });
                        match action.on_error {
                            ErrorPolicy::Continue => {}
                            ErrorPolicy::StopBinding => break,
                            ErrorPolicy::StopDispatch => {
                                report.bindings.push(binding_report);
                                break 'dispatch;
                            }
                        }
                    }
                }
            }

            report.bindings.push(binding_report);
        }

        Ok(report)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchReport {
    pub event_id: uuid::Uuid,
    pub bindings: Vec<BindingReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingReport {
    pub id: String,
    pub outcomes: Vec<crate::ActionOutcome>,
}

fn trigger_matches(pattern: &TriggerPattern, event: &InputEvent) -> bool {
    if !family_matches(&pattern.family, &event.family) {
        return false;
    }
    if !pattern.phases.is_empty() && !pattern.phases.iter().any(|phase| phase == &event.phase) {
        return false;
    }
    if !pattern.fingers.is_empty()
        && !event
            .fingers
            .is_some_and(|fingers| pattern.fingers.contains(&fingers))
    {
        return false;
    }
    if !pattern.directions.is_empty()
        && !event.direction.as_ref().is_some_and(|direction| {
            pattern
                .directions
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(direction))
        })
    {
        return false;
    }
    if pattern
        .labels
        .iter()
        .any(|(key, expected)| event.labels.get(key) != Some(expected))
    {
        return false;
    }
    if pattern.min_values.iter().any(|(key, minimum)| {
        event
            .values
            .get(key)
            .map_or(true, |actual| actual < minimum)
    }) {
        return false;
    }
    if pattern.max_values.iter().any(|(key, maximum)| {
        event
            .values
            .get(key)
            .map_or(true, |actual| actual > maximum)
    }) {
        return false;
    }
    true
}

fn family_matches(pattern: &str, actual: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return actual == prefix || actual.starts_with(&format!("{prefix}."));
    }
    pattern == actual
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn pattern() -> TriggerPattern {
        TriggerPattern {
            family: "touchpad.swipe".into(),
            phases: vec!["end".into()],
            fingers: vec![3],
            directions: vec!["up".into()],
            min_values: BTreeMap::from([("distance".into(), 100.0)]),
            max_values: BTreeMap::new(),
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn matches_a_configurable_swipe() {
        let mut event = InputEvent::new("touchpad.swipe", "end");
        event.fingers = Some(3);
        event.direction = Some("up".into());
        event.values.insert("distance".into(), 120.0);
        assert!(trigger_matches(&pattern(), &event));
    }

    #[test]
    fn rejects_insufficient_distance() {
        let mut event = InputEvent::new("touchpad.swipe", "end");
        event.fingers = Some(3);
        event.direction = Some("up".into());
        event.values.insert("distance".into(), 80.0);
        assert!(!trigger_matches(&pattern(), &event));
    }

    #[test]
    fn supports_namespaced_wildcards() {
        assert!(family_matches("touchpad.*", "touchpad.pinch"));
        assert!(!family_matches("touchpad.*", "mouse.stroke"));
    }
}
