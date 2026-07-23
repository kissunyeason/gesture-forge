//! Stable, action-agnostic contracts for GestureForge.

pub mod config;
pub mod engine;
pub mod event;
pub mod provider;

pub use config::{
    ActionSpec, Binding, ConditionSpec, Config, ErrorPolicy, RuntimeConfig, SecurityConfig,
    TriggerPattern,
};
pub use engine::{BindingReport, DispatchPlan, DispatchReport, Engine};
pub use event::{EventContext, InputEvent};
pub use provider::{
    ActionOutcome, ActionProvider, ActionRegistry, ConditionProvider, ConditionRegistry,
};
