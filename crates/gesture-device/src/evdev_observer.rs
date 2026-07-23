use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result};
use evdev::{AbsoluteAxisCode, Device, EventStream, PropType, RelativeAxisCode};
use serde::{Deserialize, Serialize};

/// Coarse device classes used only for discovery and UI hints.
///
/// Recognition never depends on these values. A user may explicitly select any
/// readable evdev node regardless of its inferred class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceClass {
    Touchpad,
    Touchscreen,
    RelativePointer,
    AbsolutePointer,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    pub keys: bool,
    pub relative_axes: bool,
    pub absolute_axes: bool,
    pub multitouch_positions: bool,
    pub pointer_property: bool,
    pub direct_property: bool,
    pub buttonpad_property: bool,
    pub semi_mt_property: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub path: PathBuf,
    pub name: String,
    pub physical_path: Option<String>,
    pub unique_name: Option<String>,
    pub class: DeviceClass,
    pub capabilities: DeviceCapabilities,
}

impl DeviceInfo {
    pub fn is_touchpad_candidate(&self) -> bool {
        self.class == DeviceClass::Touchpad
    }
}

/// A raw kernel input event captured without grabbing or modifying the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawInputEvent {
    pub timestamp_micros: Option<u128>,
    pub event_type: String,
    pub code: u16,
    pub value: i32,
    pub summary: String,
}

/// Enumerate evdev nodes that the current process can open.
///
/// The underlying evdev enumerator intentionally omits nodes that cannot be
/// opened, so an empty result can indicate either no devices or missing access.
pub fn enumerate_devices() -> Vec<DeviceInfo> {
    let mut devices: Vec<_> = evdev::enumerate()
        .map(|(path, device)| describe_device(path, &device))
        .collect();
    devices.sort_by(|left, right| left.path.cmp(&right.path));
    devices
}

pub fn inspect_device(path: impl AsRef<Path>) -> Result<DeviceInfo> {
    let path = path.as_ref();
    let device = Device::open(path)
        .with_context(|| format!("failed to open input device {}", path.display()))?;
    Ok(describe_device(path.to_path_buf(), &device))
}

/// Asynchronous monitor for a single evdev node.
///
/// Shared mode is read-only and leaves events visible to the desktop. Exclusive
/// mode calls `EVIOCGRAB`, preventing other clients from receiving events until
/// this object is dropped. Neither mode creates a uinput device or emits
/// synthetic events.
pub struct EvdevObserver {
    info: DeviceInfo,
    stream: EventStream,
    exclusive: bool,
}

impl EvdevObserver {
    /// Open a device without changing event delivery to other clients.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_grab(path, false)
    }

    /// Open a device and request exclusive event delivery with `EVIOCGRAB`.
    ///
    /// The kernel releases the grab automatically when the underlying file
    /// descriptor is closed, including normal return, Ctrl+C, panic, or process
    /// termination.
    pub fn open_exclusive(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_grab(path, true)
    }

    fn open_with_grab(path: impl AsRef<Path>, exclusive: bool) -> Result<Self> {
        let path = path.as_ref();
        let mut device = Device::open(path)
            .with_context(|| format!("failed to open input device {}", path.display()))?;
        let info = describe_device(path.to_path_buf(), &device);

        if exclusive {
            device.grab().with_context(|| {
                format!(
                    "failed to exclusively grab {}; another process may already own it",
                    path.display()
                )
            })?;
        }

        let stream = device
            .into_event_stream()
            .with_context(|| format!("failed to create event stream for {}", path.display()))?;
        Ok(Self {
            info,
            stream,
            exclusive,
        })
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    pub fn is_exclusive(&self) -> bool {
        self.exclusive
    }

    /// Release a previously acquired `EVIOCGRAB` immediately.
    ///
    /// Dropping the underlying file descriptor also releases the kernel grab,
    /// but callers should use this method before performing any potentially
    /// blocking shutdown work so the desktop regains the physical device as
    /// early as possible.
    pub fn release_grab(&mut self) -> Result<()> {
        if !self.exclusive {
            return Ok(());
        }

        self.stream.device_mut().ungrab().with_context(|| {
            format!(
                "failed to release exclusive grab on {}",
                self.info.path.display()
            )
        })?;
        self.exclusive = false;
        Ok(())
    }

    pub async fn next_event(&mut self) -> Result<RawInputEvent> {
        let event =
            self.stream.next_event().await.with_context(|| {
                format!("failed to read events from {}", self.info.path.display())
            })?;

        let timestamp_micros = event
            .timestamp()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_micros());

        Ok(RawInputEvent {
            timestamp_micros,
            event_type: format!("{:?}", event.event_type()),
            code: event.code(),
            value: event.value(),
            summary: format!("{:?}", event.destructure()),
        })
    }
}

impl Drop for EvdevObserver {
    fn drop(&mut self) {
        if self.exclusive {
            let _ = self.stream.device_mut().ungrab();
            self.exclusive = false;
        }
    }
}

fn describe_device(path: PathBuf, device: &Device) -> DeviceInfo {
    let name = device.name().unwrap_or("Unnamed input device").to_owned();
    let normalized_name = name.to_ascii_lowercase();
    let properties = device.properties();
    let absolute_axes = device.supported_absolute_axes();
    let relative_axes = device.supported_relative_axes();

    let capabilities = DeviceCapabilities {
        keys: device.supported_keys().is_some(),
        relative_axes: relative_axes.is_some(),
        absolute_axes: absolute_axes.is_some(),
        multitouch_positions: absolute_axes.is_some_and(|axes| {
            axes.contains(AbsoluteAxisCode::ABS_MT_POSITION_X)
                && axes.contains(AbsoluteAxisCode::ABS_MT_POSITION_Y)
        }),
        pointer_property: properties.contains(PropType::POINTER),
        direct_property: properties.contains(PropType::DIRECT),
        buttonpad_property: properties.contains(PropType::BUTTONPAD),
        semi_mt_property: properties.contains(PropType::SEMI_MT),
    };

    let class = classify_device(
        &normalized_name,
        &capabilities,
        relative_axes.is_some_and(|axes| {
            axes.contains(RelativeAxisCode::REL_X) && axes.contains(RelativeAxisCode::REL_Y)
        }),
    );

    DeviceInfo {
        path,
        name,
        physical_path: device.physical_path().map(str::to_owned),
        unique_name: device.unique_name().map(str::to_owned),
        class,
        capabilities,
    }
}

fn classify_device(
    normalized_name: &str,
    capabilities: &DeviceCapabilities,
    has_relative_xy: bool,
) -> DeviceClass {
    if normalized_name.contains("touchpad") || normalized_name.contains("trackpad") {
        return DeviceClass::Touchpad;
    }
    if normalized_name.contains("touchscreen") || normalized_name.contains("touch screen") {
        return DeviceClass::Touchscreen;
    }
    if capabilities.multitouch_positions
        && capabilities.pointer_property
        && !capabilities.direct_property
    {
        return DeviceClass::Touchpad;
    }
    if capabilities.multitouch_positions && capabilities.direct_property {
        return DeviceClass::Touchscreen;
    }
    if has_relative_xy {
        return DeviceClass::RelativePointer;
    }
    if capabilities.absolute_axes {
        return DeviceClass::AbsolutePointer;
    }
    DeviceClass::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities() -> DeviceCapabilities {
        DeviceCapabilities::default()
    }

    #[test]
    fn explicit_touchpad_name_wins() {
        assert_eq!(
            classify_device("example touchpad", &capabilities(), false),
            DeviceClass::Touchpad
        );
    }

    #[test]
    fn pointer_multitouch_is_a_touchpad_candidate() {
        let capabilities = DeviceCapabilities {
            multitouch_positions: true,
            pointer_property: true,
            ..DeviceCapabilities::default()
        };
        assert_eq!(
            classify_device("unknown", &capabilities, false),
            DeviceClass::Touchpad
        );
    }

    #[test]
    fn direct_multitouch_is_a_touchscreen_candidate() {
        let capabilities = DeviceCapabilities {
            multitouch_positions: true,
            direct_property: true,
            ..DeviceCapabilities::default()
        };
        assert_eq!(
            classify_device("unknown", &capabilities, false),
            DeviceClass::Touchscreen
        );
    }

    #[test]
    fn relative_xy_is_a_pointer() {
        assert_eq!(
            classify_device("unknown", &capabilities(), true),
            DeviceClass::RelativePointer
        );
    }
}
