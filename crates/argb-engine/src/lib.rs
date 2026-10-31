//! The lighting engine.
//!
//! Given a [`argb_domain::LightingProfile`] and a set of devices implementing
//! [`LightingDevice`], the [`Engine`] decides for each power state whether a
//! device runs a hardware mode or is streamed to in direct mode, and drives
//! the single shared clock that keeps every software pulse in step.

pub mod device;
pub mod engine;
pub mod pulse;

pub use device::{DeviceError, LightingDevice, Persist};
pub use engine::Engine;
pub use pulse::{DEFAULT_TICK_RATE_HZ, PulseWaveform};
