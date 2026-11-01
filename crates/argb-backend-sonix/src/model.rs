//! The keyboards this backend knows, recognized from their USB ids.

/// Sonix Technology, the MCU vendor every supported keyboard reports.
pub const SONIX_VENDOR_ID: u16 = 0x0C45;
/// Interface of the HID collection that accepts the configuration
/// commands: the plain keyboard collection on interface 0. The second
/// keyboard collection on interface 1 has the same usage and refuses
/// feature reports.
pub const CONTROL_INTERFACE: i32 = 0;
/// Usage page of that collection.
pub const CONTROL_USAGE_PAGE: u16 = 0x0001;
/// Usage of that collection (keyboard).
pub const CONTROL_USAGE: u16 = 0x0006;

/// A supported keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// Cloud Nine C959 ErgoTKL, firmware 1.17.
    CloudNineC959,
}

impl Model {
    /// Every supported model.
    pub const ALL: [Model; 1] = [Model::CloudNineC959];

    /// USB product id under [`SONIX_VENDOR_ID`].
    #[must_use]
    pub const fn product_id(self) -> u16 {
        match self {
            Model::CloudNineC959 => 0x8008,
        }
    }

    /// Marketing name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Model::CloudNineC959 => "Cloud Nine C959 ErgoTKL",
        }
    }

    /// The name the backend reports for matching: the marketing name and
    /// the USB id, so a `match_name` can use either.
    #[must_use]
    pub fn reported_name(self) -> String {
        format!(
            "{} ({:04X}:{:04X})",
            self.name(),
            SONIX_VENDOR_ID,
            self.product_id()
        )
    }

    /// The model with this USB id, if supported.
    #[must_use]
    pub fn from_usb_id(vendor_id: u16, product_id: u16) -> Option<Self> {
        if vendor_id != SONIX_VENDOR_ID {
            return None;
        }
        Self::ALL
            .into_iter()
            .find(|model| model.product_id() == product_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_are_recognized_from_usb_ids() {
        assert_eq!(
            Model::from_usb_id(0x0C45, 0x8008),
            Some(Model::CloudNineC959)
        );
        assert_eq!(Model::from_usb_id(0x0C45, 0x8009), None);
        assert_eq!(Model::from_usb_id(0x046D, 0x8008), None);
    }

    #[test]
    fn the_reported_name_carries_the_usb_id() {
        assert_eq!(
            Model::CloudNineC959.reported_name(),
            "Cloud Nine C959 ErgoTKL (0C45:8008)"
        );
    }
}
