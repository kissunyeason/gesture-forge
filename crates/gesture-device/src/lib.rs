//! Hardware abstraction boundary.
//!
//! Device discovery and the observer are deliberately read-only. Exclusive
//! proxying, uinput cloning, and recognizers will be implemented behind these
//! contracts in later milestones, keeping gestures independent from actions.

mod evdev_observer;

use anyhow::Result;
use async_trait::async_trait;
use gesture_core::InputEvent;
use tokio::sync::mpsc;

pub use evdev_observer::{
    enumerate_devices, inspect_device, DeviceCapabilities, DeviceClass, DeviceInfo, EvdevObserver,
    RawInputEvent,
};

#[derive(Debug, Clone, Default)]
pub struct BackendCapabilities {
    pub exclusive_grab: bool,
    pub multitouch_frames: bool,
    pub synthetic_keyboard: bool,
    pub synthetic_pointer: bool,
    pub synthetic_touchpad: bool,
}

#[async_trait]
pub trait InputBackend: Send {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> BackendCapabilities;
    async fn run(self: Box<Self>, sender: mpsc::Sender<InputEvent>) -> Result<()>;
}

#[async_trait]
pub trait OutputBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> BackendCapabilities;
}

pub struct NullInputBackend;

#[async_trait]
impl InputBackend for NullInputBackend {
    fn name(&self) -> &'static str {
        "null"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }

    async fn run(self: Box<Self>, _sender: mpsc::Sender<InputEvent>) -> Result<()> {
        std::future::pending::<()>().await;
        Ok(())
    }
}
