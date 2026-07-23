//! Action-agnostic recognition of normalized touch frames.

mod config;
mod session;

pub use config::{
    HoldRecognizerConfig, RecognitionSettings, RecognizerConfig, SwipeRecognizerConfig,
};
pub use session::{GestureRecognizer, GestureSessionMetrics};
