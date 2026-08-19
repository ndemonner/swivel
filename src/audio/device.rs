//! Choosing and describing audio devices.
//!
//! Everything downstream assumes 48 kHz. This module is where that assumption
//! is checked, and where a device that cannot meet it is reported rather than
//! hidden.

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{BufferSize, Device, DeviceType, StreamConfig, SupportedBufferSize};

use crate::config::{DEVICE_BUFFER_FRAMES, SAMPLE_RATE};
use crate::error::{Error, Result};

/// Which way audio flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

/// Whether the output will be heard by the microphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoRisk {
    /// The voice unit is cancelling the echo, so a loudspeaker is fine.
    Cancelled,
    /// A loudspeaker. The far end will hear itself.
    Likely,
    /// Headphones or a headset. Nothing leaks back.
    Unlikely,
    /// CoreAudio did not say what this device is. Most aggregate and virtual
    /// devices land here, and so do some plain USB interfaces.
    Unknown,
}

/// A chosen device and the configuration to open it with.
pub struct Chosen {
    pub device: Device,
    pub name: String,
    pub config: StreamConfig,
    /// The device runs at 48 kHz. When false, the pipeline needs a resampler
    /// and the latency budget no longer holds.
    pub native_rate: bool,
    /// Whether the far end is likely to hear itself.
    pub echo: EchoRisk,
}

impl Chosen {
    /// The device buffer delay, in milliseconds.
    pub fn buffer_ms(&self) -> f32 {
        match self.config.buffer_size {
            BufferSize::Fixed(frames) => frames as f32 * 1000.0 / self.config.sample_rate as f32,
            BufferSize::Default => f32::NAN,
        }
    }
}

/// Picks a device for a direction and works out how to open it.
///
/// `preferred` names a device the user chose. A name that no longer matches
/// anything falls back to the system default and says so, because a device that
/// went away must not stop the application from starting.
pub fn choose(direction: Direction, preferred: Option<&str>) -> Result<Chosen> {
    let host = cpal::default_host();

    let device = preferred
        .and_then(|name| find_by_name(&host, direction, name))
        .or_else(|| {
            if let Some(name) = preferred {
                tracing::warn!(
                    "the chosen {direction:?} device {name:?} is not here, so the system \
                     default is used"
                );
            }
            match direction {
                Direction::Input => host.default_input_device(),
                Direction::Output => host.default_output_device(),
            }
        })
        .ok_or_else(|| Error::Audio(format!("there is no {direction:?} device")))?;

    let description = device.description().map_err(Error::audio)?;
    let name = description.name().to_string();

    // Only report a real speaker. CoreAudio reports many perfectly good
    // headphone interfaces as `Unknown`, and a warning that fires on the
    // correct setup teaches the user to ignore warnings.
    let echo = match (direction, description.device_type()) {
        (Direction::Input, _) => EchoRisk::Unlikely,
        (_, DeviceType::Speaker) => EchoRisk::Likely,
        (_, DeviceType::Headphones | DeviceType::Headset | DeviceType::Earpiece) => {
            EchoRisk::Unlikely
        }
        _ => EchoRisk::Unknown,
    };

    let configs: Vec<_> = match direction {
        Direction::Input => device
            .supported_input_configs()
            .map_err(Error::audio)?
            .collect(),
        Direction::Output => device
            .supported_output_configs()
            .map_err(Error::audio)?
            .collect(),
    };

    if configs.is_empty() {
        return Err(Error::Audio(format!("{name} reports no usable format")));
    }

    // Prefer 48 kHz. Everything downstream is built for it, and a resampler in
    // the hot path costs latency that cannot be recovered.
    let native = configs
        .iter()
        .find(|c| c.min_sample_rate() <= SAMPLE_RATE && SAMPLE_RATE <= c.max_sample_rate());

    let (chosen, rate, native_rate) = match native {
        Some(c) => (c, SAMPLE_RATE, true),
        None => {
            // Fall back to the highest rate the device offers, and say so.
            let c = configs
                .iter()
                .max_by_key(|c| c.max_sample_rate())
                .expect("the list is not empty");
            (c, c.max_sample_rate(), false)
        }
    };

    let channels = chosen.channels().max(1);
    let buffer_size = pick_buffer_size(chosen.buffer_size());

    Ok(Chosen {
        device,
        name,
        config: StreamConfig {
            channels,
            sample_rate: rate,
            buffer_size,
        },
        native_rate,
        echo,
    })
}

/// Picks the smallest safe buffer the device allows.
///
/// A smaller buffer is less latency and more risk of a dropout. The default is
/// clamped into whatever range the device reports rather than trusted blindly,
/// because CoreAudio refuses a stream with a size it did not offer.
fn pick_buffer_size(supported: &SupportedBufferSize) -> BufferSize {
    match supported {
        SupportedBufferSize::Range { min, max } => {
            BufferSize::Fixed(DEVICE_BUFFER_FRAMES.clamp(*min, *max))
        }
        // Some backends will not say. Let the backend decide and report the
        // unknown delay in `swivel doctor`.
        _ => BufferSize::Default,
    }
}

/// Finds a device by its exact name.
fn find_by_name(host: &cpal::Host, direction: Direction, name: &str) -> Option<Device> {
    let devices = match direction {
        Direction::Input => host.input_devices().ok()?,
        Direction::Output => host.output_devices().ok()?,
    };

    devices.into_iter().find(|d| {
        d.description()
            .map(|desc| desc.name() == name)
            .unwrap_or(false)
    })
}

/// Every device name for a direction, in the order the host reports them.
pub fn names(direction: Direction) -> Vec<String> {
    let host = cpal::default_host();
    let devices = match direction {
        Direction::Input => host.input_devices().ok(),
        Direction::Output => host.output_devices().ok(),
    };

    devices
        .map(|list| {
            list.into_iter()
                .filter_map(|d| d.description().ok().map(|desc| desc.name().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// The name of the current system default for a direction.
pub fn default_name(direction: Direction) -> Option<String> {
    let host = cpal::default_host();
    let device = match direction {
        Direction::Input => host.default_input_device(),
        Direction::Output => host.default_output_device(),
    }?;
    device.description().ok().map(|d| d.name().to_string())
}

/// Lists every device, for `swivel doctor`.
pub fn describe_all() -> Vec<(Direction, String, bool)> {
    let host = cpal::default_host();
    let mut out = Vec::new();

    if let Ok(devices) = host.input_devices() {
        for d in devices {
            if let Ok(desc) = d.description() {
                out.push((
                    Direction::Input,
                    desc.name().to_string(),
                    supports_48k(&d, Direction::Input),
                ));
            }
        }
    }
    if let Ok(devices) = host.output_devices() {
        for d in devices {
            if let Ok(desc) = d.description() {
                out.push((
                    Direction::Output,
                    desc.name().to_string(),
                    supports_48k(&d, Direction::Output),
                ));
            }
        }
    }
    out
}

fn supports_48k(device: &Device, direction: Direction) -> bool {
    let configs = match direction {
        Direction::Input => device
            .supported_input_configs()
            .map(|c| c.collect::<Vec<_>>()),
        Direction::Output => device
            .supported_output_configs()
            .map(|c| c.collect::<Vec<_>>()),
    };

    configs
        .map(|list| {
            list.iter()
                .any(|c| c.min_sample_rate() <= SAMPLE_RATE && SAMPLE_RATE <= c.max_sample_rate())
        })
        .unwrap_or(false)
}

/// The device name a direction really opens.
///
/// `preferred` is the stored choice. A stored name that the host no longer
/// offers falls back to the system default, because `choose` falls back the
/// same way. The menu tick has to agree with the audio path, or it reports a
/// device that is not in use.
pub fn in_use(
    preferred: Option<&str>,
    offered: &[String],
    default: Option<&str>,
) -> Option<String> {
    match preferred {
        Some(name) if offered.iter().any(|n| n == name) => Some(name.to_string()),
        _ => default.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered() -> Vec<String> {
        vec!["MacBook Pro Microphone".into(), "Scarlett Solo".into()]
    }

    #[test]
    fn a_stored_device_that_is_here_is_the_one_in_use() {
        let name = in_use(
            Some("Scarlett Solo"),
            &offered(),
            Some("MacBook Pro Microphone"),
        );
        assert_eq!(name.as_deref(), Some("Scarlett Solo"));
    }

    #[test]
    fn a_stored_device_that_went_away_falls_back_to_the_default() {
        let name = in_use(
            Some("Blue Yeti"),
            &offered(),
            Some("MacBook Pro Microphone"),
        );
        assert_eq!(name.as_deref(), Some("MacBook Pro Microphone"));
    }

    #[test]
    fn no_stored_device_is_the_system_default() {
        let name = in_use(None, &offered(), Some("MacBook Pro Microphone"));
        assert_eq!(name.as_deref(), Some("MacBook Pro Microphone"));
    }

    #[test]
    fn a_machine_with_no_device_at_all_has_none_in_use() {
        assert_eq!(in_use(None, &[], None), None);
        assert_eq!(in_use(Some("Blue Yeti"), &[], None), None);
    }
}
