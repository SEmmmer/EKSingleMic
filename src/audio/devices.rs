use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub name: String,
    pub default_channels: Option<u16>,
    pub default_sample_rate_hz: Option<u32>,
}

impl DeviceDescriptor {
    pub fn summary(&self) -> String {
        match (self.default_channels, self.default_sample_rate_hz) {
            (Some(channels), Some(sample_rate)) => {
                format!("{} ({} ch / {} Hz)", self.name, channels, sample_rate)
            }
            _ => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualCableRoute {
    pub driver_name: &'static str,
    pub playback_device_name: String,
    pub recording_device_name: String,
}

#[derive(Debug, Default, Clone)]
pub struct DeviceInventory {
    pub host_name: String,
    pub input_devices: Vec<DeviceDescriptor>,
    pub output_devices: Vec<DeviceDescriptor>,
    pub virtual_cable_route: Option<VirtualCableRoute>,
}

pub fn enumerate_audio_devices() -> Result<DeviceInventory> {
    let host = cpal::default_host();
    let host_name = format!("{:?}", host.id());

    let input_devices: Vec<DeviceDescriptor> = host
        .input_devices()
        .context("failed to enumerate input devices")?
        .map(|device| describe_input_device(&device))
        .collect();

    let output_devices: Vec<DeviceDescriptor> = host
        .output_devices()
        .context("failed to enumerate output devices")?
        .map(|device| describe_output_device(&device))
        .collect();
    let virtual_cable_route = detect_virtual_cable_route(&input_devices, &output_devices);

    Ok(DeviceInventory {
        host_name,
        input_devices,
        output_devices,
        virtual_cable_route,
    })
}

pub fn find_input_device(name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();

    if let Some(name) = normalize_device_name(name) {
        host.input_devices()
            .context("failed to enumerate input devices while resolving selection")?
            .find(|device| {
                device
                    .name()
                    .map(|candidate| candidate == name)
                    .unwrap_or(false)
            })
            .ok_or_else(|| anyhow!("failed to find selected input device: {name}"))
    } else {
        host.default_input_device()
            .ok_or_else(|| anyhow!("no default input device is available"))
    }
}

pub fn find_output_device(name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();

    if let Some(name) = normalize_device_name(name) {
        host.output_devices()
            .context("failed to enumerate output devices while resolving selection")?
            .find(|device| {
                device
                    .name()
                    .map(|candidate| candidate == name)
                    .unwrap_or(false)
            })
            .ok_or_else(|| anyhow!("failed to find selected output device: {name}"))
    } else {
        host.default_output_device()
            .ok_or_else(|| anyhow!("no default output device is available"))
    }
}

fn describe_input_device(device: &cpal::Device) -> DeviceDescriptor {
    let name = device
        .name()
        .unwrap_or_else(|_| "Unknown input device".to_owned());
    let default_config = device.default_input_config().ok();

    DeviceDescriptor {
        name,
        default_channels: default_config.as_ref().map(|config| config.channels()),
        default_sample_rate_hz: default_config.as_ref().map(|config| config.sample_rate().0),
    }
}

fn describe_output_device(device: &cpal::Device) -> DeviceDescriptor {
    let name = device
        .name()
        .unwrap_or_else(|_| "Unknown output device".to_owned());
    let default_config = device.default_output_config().ok();

    DeviceDescriptor {
        name,
        default_channels: default_config.as_ref().map(|config| config.channels()),
        default_sample_rate_hz: default_config.as_ref().map(|config| config.sample_rate().0),
    }
}

fn normalize_device_name(name: Option<&str>) -> Option<&str> {
    name.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn detect_virtual_cable_route(
    input_devices: &[DeviceDescriptor],
    output_devices: &[DeviceDescriptor],
) -> Option<VirtualCableRoute> {
    let playback_device = output_devices
        .iter()
        .find(|device| is_vb_cable_playback_device(&device.name))?;
    let recording_device = input_devices
        .iter()
        .find(|device| is_vb_cable_recording_device(&device.name))?;

    Some(VirtualCableRoute {
        driver_name: "VB-CABLE",
        playback_device_name: playback_device.name.clone(),
        recording_device_name: recording_device.name.clone(),
    })
}

fn is_vb_cable_playback_device(name: &str) -> bool {
    let normalized = normalize_for_match(name);
    normalized.contains("vb-audio") && normalized.contains("cable") && normalized.contains("input")
}

fn is_vb_cable_recording_device(name: &str) -> bool {
    let normalized = normalize_for_match(name);
    normalized.contains("vb-audio") && normalized.contains("cable") && normalized.contains("output")
}

fn normalize_for_match(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}
