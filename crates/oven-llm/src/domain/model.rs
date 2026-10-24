//! `ModelId`: the model id.

use std::borrow::Borrow;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The model id: a full slug (`vendor/wire-id[:variant]`) or a bare wire id.
///
/// serde still sees a plain string. Only [`wire_id`](Self::wire_id) goes
/// upstream — the `vendor/` prefix never reaches the request body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The string it was built from: a slug, or a bare id.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The vendor part of the slug, not canonicalized. A bare id has none.
    pub fn vendor(&self) -> Option<&str> {
        let (vendor, _, _) = split_model_id(&self.0);
        vendor
    }

    /// The model name the vendor reads: the vendor prefix and `:variant` are
    /// gone.
    pub fn wire_id(&self) -> &str {
        let (_, wire, _) = split_model_id(&self.0);
        wire
    }

    /// The protocol variant, e.g. `responses`. The picker does not show it; it
    /// is for a slug typed by hand.
    pub fn variant(&self) -> Option<&str> {
        let (_, _, variant) = split_model_id(&self.0);
        variant
    }

    /// Fills a bare id into `vendor/wire-id[:variant]`. An id that already
    /// has a vendor keeps its wire id and its variant.
    pub fn qualify(&self, vendor: &str) -> Self {
        let vendor = canonical_vendor(vendor);
        match self.variant() {
            Some(variant) => Self(format!("{vendor}/{}:{variant}", self.wire_id())),
            None => Self(format!("{vendor}/{}", self.wire_id())),
        }
    }
}

/// Known vendor aliases → the canonical slug (lowercase). Anything else comes
/// back lowercased, as it was.
pub fn canonical_vendor(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "kimi" | "moonshot" => "moonshot".to_string(),
        "glm" | "zhipu" => "zhipu".to_string(),
        "grok" | "xai" => "xai".to_string(),
        other => other.to_lowercase(),
    }
}

fn split_model_id(raw: &str) -> (Option<&str>, &str, Option<&str>) {
    let (main, variant) = match raw.rsplit_once(':') {
        Some((left, right)) if !left.is_empty() && !right.is_empty() && !right.contains('/') => {
            (left, Some(right))
        }
        _ => (raw, None),
    };
    match main.split_once('/') {
        Some((vendor, wire)) if !vendor.is_empty() && !wire.is_empty() => {
            (Some(vendor), wire, variant)
        }
        _ => (None, main, variant),
    }
}

impl From<String> for ModelId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ModelId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl AsRef<str> for ModelId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for ModelId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_id_parses_slug_variant_and_bare() {
        let slug = ModelId::from("deepseek/deepseek-v4-flash:responses");
        assert_eq!(slug.vendor(), Some("deepseek"));
        assert_eq!(slug.wire_id(), "deepseek-v4-flash");
        assert_eq!(slug.variant(), Some("responses"));
        assert_eq!(slug.as_str(), "deepseek/deepseek-v4-flash:responses");

        let nested = ModelId::from("my-proxy/org/model");
        assert_eq!(nested.vendor(), Some("my-proxy"));
        assert_eq!(nested.wire_id(), "org/model");
        assert_eq!(nested.variant(), None);

        let bare = ModelId::from("deepseek-v4-flash");
        assert_eq!(bare.vendor(), None);
        assert_eq!(bare.wire_id(), "deepseek-v4-flash");
        assert_eq!(bare.variant(), None);

        let bare_variant = ModelId::from("deepseek-v4-flash:responses");
        assert_eq!(bare_variant.vendor(), None);
        assert_eq!(bare_variant.wire_id(), "deepseek-v4-flash");
        assert_eq!(bare_variant.variant(), Some("responses"));
    }

    #[test]
    fn model_id_qualify_and_canonical_vendor() {
        let qualified = ModelId::from("deepseek-v4-flash:responses").qualify("kimi");
        assert_eq!(qualified.as_str(), "moonshot/deepseek-v4-flash:responses");
        assert_eq!(canonical_vendor("GROK"), "xai");
        assert_eq!(canonical_vendor("glm"), "zhipu");
        assert_eq!(canonical_vendor("My-Proxy"), "my-proxy");

        let rewritten = ModelId::from("grok/grok-4.6").qualify("xai");
        assert_eq!(rewritten.as_str(), "xai/grok-4.6");
    }
}
