//! Action-agnostic recognition of normalized touch frames.

mod config;
mod session;

pub use config::{
    DragRuleConfig, HoldRecognizerConfig, HoldRuleConfig, RecognitionSettings, RecognizerConfig,
    SwipeRecognizerConfig, SwipeRuleConfig,
};
pub use session::{GestureRecognizer, GestureSessionMetrics};
