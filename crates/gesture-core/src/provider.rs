use std::{collections::HashMap, sync::Arc};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{ActionSpec, ConditionSpec, InputEvent};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionOutcome {
    pub provider: String,
    pub action: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ActionOutcome {
    pub fn success(spec: &ActionSpec, message: impl Into<Option<String>>) -> Self {
        Self {
            provider: spec.provider.clone(),
            action: spec.action.clone(),
            success: true,
            message: message.into(),
        }
    }
}

#[async_trait]
pub trait ActionProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn validate(&self, spec: &ActionSpec) -> Result<()>;
    async fn execute(&self, spec: &ActionSpec, event: &InputEvent) -> Result<ActionOutcome>;
}

#[async_trait]
pub trait ConditionProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn validate(&self, spec: &ConditionSpec) -> Result<()>;
    async fn evaluate(&self, spec: &ConditionSpec, event: &InputEvent) -> Result<bool>;
}

#[derive(Default)]
pub struct ActionRegistry {
    providers: HashMap<String, Arc<dyn ActionProvider>>,
}

impl ActionRegistry {
    pub fn register<P>(&mut self, provider: P) -> Result<()>
    where
        P: ActionProvider + 'static,
    {
        let name = provider.name().to_owned();
        if self.providers.insert(name.clone(), Arc::new(provider)).is_some() {
            bail!("action provider {name:?} is already registered");
        }
        Ok(())
    }

    pub fn validate(&self, spec: &ActionSpec) -> Result<()> {
        self.provider(&spec.provider)?.validate(spec)
    }

    pub async fn execute(&self, spec: &ActionSpec, event: &InputEvent) -> Result<ActionOutcome> {
        self.provider(&spec.provider)?
            .execute(spec, event)
            .await
            .with_context(|| format!("action {}.{} failed", spec.provider, spec.action))
    }

    fn provider(&self, name: &str) -> Result<&Arc<dyn ActionProvider>> {
        self.providers
            .get(name)
            .with_context(|| format!("unknown action provider {name:?}"))
    }
}

#[derive(Default)]
pub struct ConditionRegistry {
    providers: HashMap<String, Arc<dyn ConditionProvider>>,
}

impl ConditionRegistry {
    pub fn register<P>(&mut self, provider: P) -> Result<()>
    where
        P: ConditionProvider + 'static,
    {
        let name = provider.name().to_owned();
        if self.providers.insert(name.clone(), Arc::new(provider)).is_some() {
            bail!("condition provider {name:?} is already registered");
        }
        Ok(())
    }

    pub fn validate(&self, spec: &ConditionSpec) -> Result<()> {
        self.provider(&spec.provider)?.validate(spec)
    }

    pub async fn evaluate(&self, spec: &ConditionSpec, event: &InputEvent) -> Result<bool> {
        let result = self.provider(&spec.provider)?.evaluate(spec, event).await?;
        Ok(if spec.negate { !result } else { result })
    }

    fn provider(&self, name: &str) -> Result<&Arc<dyn ConditionProvider>> {
        self.providers
            .get(name)
            .with_context(|| format!("unknown condition provider {name:?}"))
    }
}
