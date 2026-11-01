//! Opening a connection and wrapping every controller as a device.

use std::sync::{Arc, Mutex};

use argb_domain::LightingProfile;
use tracing::info;

use crate::client::{OpenRgbClient, OpenRgbError};
use crate::device::{OpenRgbDevice, SharedClient};
use crate::discovery::assign_aliases;

/// A connection and the devices built on it.
pub struct OpenRgbSession {
    /// The connection every device writes through. The service also reads
    /// server notifications from it between writes.
    pub client: SharedClient<OpenRgbClient>,
    /// One device per controller the server listed when the session opened.
    pub devices: Vec<OpenRgbDevice<OpenRgbClient>>,
}

/// Like [`open_session`], returning only the devices. For callers that
/// never need the connection itself.
///
/// # Errors
/// [`OpenRgbError`] when the server is unreachable or answers badly.
pub fn open_devices(
    host: &str,
    port: u16,
    client_name: &str,
    profile: &LightingProfile,
) -> Result<Vec<OpenRgbDevice<OpenRgbClient>>, OpenRgbError> {
    open_session(host, port, client_name, profile).map(|session| session.devices)
}

/// Connects to the server at `host:port` as `client_name`, lists its
/// controllers and wraps each as a device under the alias the profile gives
/// it (or a derived one). The service and the CLI share this so they drive
/// the same device set.
///
/// # Errors
/// [`OpenRgbError`] when the server is unreachable or answers badly.
pub fn open_session(
    host: &str,
    port: u16,
    client_name: &str,
    profile: &LightingProfile,
) -> Result<OpenRgbSession, OpenRgbError> {
    let mut client = OpenRgbClient::connect(host, port, client_name)?;
    let controllers = client.controllers()?;
    let client = Arc::new(Mutex::new(client));
    let devices = assign_aliases(profile, &controllers)
        .into_iter()
        .map(|assignment| {
            let description = controllers[assignment.index as usize].clone();
            info!(
                index = assignment.index,
                alias = %assignment.alias,
                name = ?description.name,
                configured = assignment.configured,
                "controller mapped"
            );
            OpenRgbDevice::new(
                assignment.alias,
                assignment.index,
                description,
                Arc::clone(&client),
            )
        })
        .collect();
    Ok(OpenRgbSession { client, devices })
}
