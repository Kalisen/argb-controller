//! The HID link to one keyboard, and the write session over it.

use std::ffi::CString;

use hidapi::{HidApi, HidDevice};
use tracing::debug;

use crate::error::SonixError;
use crate::model::{CONTROL_INTERFACE, CONTROL_USAGE, CONTROL_USAGE_PAGE, Model};
use crate::protocol::{ACK_RETRIES, Command, PACKET_GAP, Report, acknowledged, command};

/// What the backend needs from a keyboard connection, so devices can be
/// tested against a fake.
pub trait Link {
    /// Finds the keyboard and opens its control collection. Idempotent
    /// while open.
    ///
    /// # Errors
    /// [`SonixError`] when no keyboard matches or it cannot be opened.
    fn open(&mut self) -> Result<(), SonixError>;

    /// Writes one feature report, then waits the pacing the firmware needs.
    ///
    /// # Errors
    /// [`SonixError`] when not open or the write fails.
    fn send(&mut self, report: &Report) -> Result<(), SonixError>;

    /// Reads one feature report: the keyboard's echo of the last command.
    ///
    /// # Errors
    /// [`SonixError`] when not open or the read fails.
    fn receive(&mut self) -> Result<Report, SonixError>;

    /// Drops the handle; the next [`Self::open`] finds the keyboard again.
    fn close(&mut self);

    /// Whether the handle is open.
    fn is_open(&self) -> bool;
}

/// A keyboard plugged in, as the HID enumeration shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    /// The supported model.
    pub model: Model,
    /// Path of the control collection.
    pub path: CString,
}

/// Every supported keyboard plugged in, by its control collection.
///
/// # Errors
/// [`SonixError::Hid`] when the HID library cannot enumerate.
pub fn attached_keyboards() -> Result<Vec<Attached>, SonixError> {
    let api = HidApi::new().map_err(|e| SonixError::Hid(e.to_string()))?;
    Ok(api
        .device_list()
        .filter(|info| {
            info.interface_number() == CONTROL_INTERFACE
                && info.usage_page() == CONTROL_USAGE_PAGE
                && info.usage() == CONTROL_USAGE
        })
        .filter_map(|info| {
            Model::from_usb_id(info.vendor_id(), info.product_id()).map(|model| Attached {
                model,
                path: info.path().to_owned(),
            })
        })
        .collect())
}

/// The link to the keyboard whose reported name contains `match_name`,
/// opened on first use so an unplugged keyboard is found once replugged.
pub struct HidLink {
    match_name: String,
    device: Option<HidDevice>,
}

impl HidLink {
    /// A link for a configured selector.
    #[must_use]
    pub fn new(match_name: &str) -> Self {
        Self {
            match_name: match_name.to_owned(),
            device: None,
        }
    }

    /// The selector this link resolves.
    #[must_use]
    pub fn match_name(&self) -> &str {
        &self.match_name
    }

    /// Whether `name` is what this link looks for.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        name.to_ascii_lowercase()
            .contains(&self.match_name.to_ascii_lowercase())
    }
}

impl Link for HidLink {
    fn open(&mut self) -> Result<(), SonixError> {
        if self.device.is_some() {
            return Ok(());
        }
        let attached = attached_keyboards()?
            .into_iter()
            .find(|keyboard| self.matches(&keyboard.model.reported_name()))
            .ok_or_else(|| SonixError::NotFound {
                match_name: self.match_name.clone(),
            })?;
        let api = HidApi::new().map_err(|e| SonixError::Hid(e.to_string()))?;
        let device = api
            .open_path(&attached.path)
            .map_err(|e| SonixError::at("opening the keyboard", e))?;
        debug!(model = attached.model.name(), "keyboard opened");
        self.device = Some(device);
        Ok(())
    }

    fn send(&mut self, report: &Report) -> Result<(), SonixError> {
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| SonixError::at("writing", "keyboard not open"))?;
        device
            .send_feature_report(report)
            .map_err(|e| SonixError::at("writing a feature report", e))?;
        std::thread::sleep(PACKET_GAP);
        Ok(())
    }

    fn receive(&mut self) -> Result<Report, SonixError> {
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| SonixError::at("reading", "keyboard not open"))?;
        let mut echo = [0; crate::protocol::REPORT_LEN];
        device
            .get_feature_report(&mut echo)
            .map_err(|e| SonixError::at("reading a feature report", e))?;
        Ok(echo)
    }

    fn close(&mut self) {
        self.device = None;
    }

    fn is_open(&self) -> bool {
        self.device.is_some()
    }
}

/// Sends a command until the keyboard acknowledges it.
fn exchange_acked<L: Link>(link: &mut L, report: &Report) -> Result<(), SonixError> {
    for _ in 0..ACK_RETRIES {
        link.send(report)?;
        if acknowledged(&link.receive()?) {
            return Ok(());
        }
    }
    Err(SonixError::NotAcknowledged {
        command: report[2],
        attempts: ACK_RETRIES,
    })
}

/// One write session: begin, select `section` with its page count, the
/// pages, commit, apply. Opens the link first; closes it on failure so the
/// next attempt starts from a fresh handle.
///
/// # Errors
/// [`SonixError`] from the link, or when a command is not acknowledged.
pub fn write_section<L: Link>(
    link: &mut L,
    section: Command,
    pages: &[Report],
) -> Result<(), SonixError> {
    let count = u8::try_from(pages.len())
        .map_err(|_| SonixError::at("preparing a section", "too many pages"))?;
    let result = link.open().and_then(|()| {
        exchange_acked(link, &command(Command::Begin, 0))?;
        exchange_acked(link, &command(section, count))?;
        for page in pages {
            link.send(page)?;
        }
        exchange_acked(link, &command(Command::Commit, 0))?;
        exchange_acked(link, &command(Command::Apply, 0))
    });
    if result.is_err() {
        link.close();
    }
    result
}

/// A link that records what it is sent and answers like the firmware.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;

    /// How the fake answers commands.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Answer {
        /// Acknowledge every command.
        Ack,
        /// Refuse every command.
        Refuse,
    }

    pub(crate) struct FakeLink {
        pub(crate) open: bool,
        pub(crate) opens: usize,
        pub(crate) sent: Vec<Report>,
        pub(crate) answer: Answer,
        /// Fail the next open with "not found".
        pub(crate) missing: bool,
    }

    impl FakeLink {
        pub(crate) fn new() -> Self {
            Self {
                open: false,
                opens: 0,
                sent: Vec::new(),
                answer: Answer::Ack,
                missing: false,
            }
        }

        /// The command bytes sent, in order, data pages shown as `None`.
        pub(crate) fn commands(&self) -> Vec<Option<u8>> {
            self.sent
                .iter()
                .map(|report| (report[1] == crate::protocol::COMMAND_MARKER).then_some(report[2]))
                .collect()
        }
    }

    impl Link for FakeLink {
        fn open(&mut self) -> Result<(), SonixError> {
            if self.missing {
                return Err(SonixError::NotFound {
                    match_name: "fake".into(),
                });
            }
            if !self.open {
                self.opens += 1;
                self.open = true;
            }
            Ok(())
        }

        fn send(&mut self, report: &Report) -> Result<(), SonixError> {
            assert!(self.open, "sent while closed");
            self.sent.push(*report);
            Ok(())
        }

        fn receive(&mut self) -> Result<Report, SonixError> {
            let mut echo = *self.sent.last().expect("a command was sent");
            echo[1 + crate::protocol::ACK_OFFSET] = match self.answer {
                Answer::Ack => crate::protocol::ACK,
                Answer::Refuse => 0xFF,
            };
            Ok(echo)
        }

        fn close(&mut self) {
            self.open = false;
        }

        fn is_open(&self) -> bool {
            self.open
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Answer, FakeLink};
    use super::*;
    use crate::protocol::LightingBlock;

    #[test]
    fn a_session_is_begin_select_pages_commit_apply() {
        let mut link = FakeLink::new();
        let pages = LightingBlock::off().pages();
        write_section(&mut link, Command::Lighting, &pages).unwrap();
        let mut expected = vec![Some(0x18), Some(0x13)];
        expected.extend(std::iter::repeat_n(None, LightingBlock::PAGES));
        expected.extend([Some(0x02), Some(0xF0)]);
        assert_eq!(link.commands(), expected);
        assert_eq!(link.sent[1][9], 0x12, "the select carries the page count");
        assert!(link.is_open());
    }

    #[test]
    fn a_refused_command_is_retried_then_fails_and_closes_the_link() {
        let mut link = FakeLink::new();
        link.answer = Answer::Refuse;
        let error = write_section(&mut link, Command::Lighting, &[]).unwrap_err();
        assert_eq!(
            error,
            SonixError::NotAcknowledged {
                command: 0x18,
                attempts: ACK_RETRIES
            }
        );
        assert_eq!(link.sent.len(), ACK_RETRIES);
        assert!(!link.is_open());
    }

    #[test]
    fn a_missing_keyboard_is_reported_without_sending() {
        let mut link = FakeLink::new();
        link.missing = true;
        assert!(matches!(
            write_section(&mut link, Command::Lighting, &[]),
            Err(SonixError::NotFound { .. })
        ));
        assert!(link.sent.is_empty());
    }

    #[test]
    fn hid_link_matches_names_case_insensitively() {
        let link = HidLink::new("c959");
        assert!(link.matches(&Model::CloudNineC959.reported_name()));
        assert!(!link.matches("Logitech G203"));
        assert!(!link.is_open());
    }
}
