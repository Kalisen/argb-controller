//! The Govee models this backend knows, recognized from the advertised name.

use crate::protocol::Layout;

/// Prefix of the advertised name of current Govee lights.
pub const GOVEE_NAME_PREFIX: &str = "Govee_";
/// Prefix older Govee firmware advertises under.
pub const IHOMENT_NAME_PREFIX: &str = "ihoment_";
/// Length of a model code such as `H6061`.
const MODEL_CODE_LEN: usize = 5;

/// A supported model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// Glide Hexa light panels, ten panels.
    GlideHexa,
    /// Lyra floor lamp, seven segments.
    LyraFloorLamp,
}

impl Model {
    /// Model code as it appears in the advertised name.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Model::GlideHexa => "H6061",
            Model::LyraFloorLamp => "H6076",
        }
    }

    /// Frame layout the model expects.
    #[must_use]
    pub const fn layout(self) -> Layout {
        match self {
            Model::GlideHexa => Layout::Hexa,
            Model::LyraFloorLamp => Layout::Lamp,
        }
    }

    /// Every supported model.
    pub const ALL: [Model; 2] = [Model::GlideHexa, Model::LyraFloorLamp];

    /// The model whose code appears in `text` (an advertised name such as
    /// `Govee_H6061_7867`, or a configured `match_name`), if supported.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let upper = text.to_ascii_uppercase();
        Self::ALL
            .into_iter()
            .find(|model| upper.contains(model.code()))
    }
}

/// Whether an advertised name belongs to a Govee light at all, supported or not.
#[must_use]
pub fn is_govee_name(name: &str) -> bool {
    name.starts_with(GOVEE_NAME_PREFIX) || name.starts_with(IHOMENT_NAME_PREFIX)
}

/// The model code (`Hxxxx`) in an advertised Govee name, supported or not.
#[must_use]
pub fn model_code(name: &str) -> Option<&str> {
    name.split('_')
        .find(|part| part.len() == MODEL_CODE_LEN && part.starts_with('H'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_are_recognized_from_names_and_selectors() {
        assert_eq!(Model::from_text("Govee_H6061_7867"), Some(Model::GlideHexa));
        assert_eq!(Model::from_text("h6076"), Some(Model::LyraFloorLamp));
        assert_eq!(Model::from_text("Govee_H5151_2737"), None);
    }

    #[test]
    fn govee_names_and_model_codes() {
        assert!(is_govee_name("Govee_H6076_6539"));
        assert!(is_govee_name("ihoment_H6110_C211"));
        assert!(!is_govee_name("GVH5100_2B98"));
        assert_eq!(model_code("Govee_H5151_2737"), Some("H5151"));
        assert_eq!(model_code("ESP32"), None);
    }
}
