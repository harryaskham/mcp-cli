//! Tool annotations and rich result content (MCP 2025-03-26 / 2025-06-18).
//!
//! A tool's JSON envelope stays the `structuredContent` and the first `text`
//! block, exactly as before. Extra [`ContentBlock`]s — images, audio, resource
//! links and embedded resources — are appended after it and **only** there: their
//! bytes never enter the envelope, so a client or transcript that keeps only the
//! structured result does not silently retain an image or a document.

use serde::{Deserialize, Serialize};

/// Behaviour hints a client may use to present or gate a tool.
///
/// These are hints, not guarantees: MCP clients must not treat them as a
/// security boundary, and a server must still authorise every call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    /// Human-readable title for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The tool does not modify its environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    /// The tool may perform destructive updates (meaningful when not read-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    /// Repeating a call with the same arguments has no additional effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    /// The tool interacts with an open world of external entities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

impl ToolAnnotations {
    /// Annotations for a tool that only reads.
    #[must_use]
    pub fn read_only() -> Self {
        Self {
            read_only_hint: Some(true),
            ..Self::default()
        }
    }

    /// Annotations for a tool that changes state.
    #[must_use]
    pub fn mutating(destructive: bool, idempotent: bool) -> Self {
        Self {
            read_only_hint: Some(false),
            destructive_hint: Some(destructive),
            idempotent_hint: Some(idempotent),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    #[must_use]
    pub const fn with_open_world(mut self, open_world: bool) -> Self {
        self.open_world_hint = Some(open_world);
        self
    }
}

/// The resource carried by an embedded-resource block: exactly one of `text` or
/// `blob` (base64).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedResource {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

/// An MCP content block returned after a tool's envelope `text` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Base64 image data, e.g. a screenshot.
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    /// Base64 audio data.
    Audio {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    /// A link the client may fetch or open; no bytes travel in the result.
    ResourceLink {
        uri: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, rename = "mimeType", skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },
    /// A resource whose contents travel in the result.
    Resource { resource: EmbeddedResource },
}

impl ContentBlock {
    /// An image block from raw bytes.
    #[must_use]
    pub fn image(bytes: &[u8], mime_type: impl Into<String>) -> Self {
        Self::Image {
            data: base64_encode(bytes),
            mime_type: mime_type.into(),
        }
    }

    /// An audio block from raw bytes.
    #[must_use]
    pub fn audio(bytes: &[u8], mime_type: impl Into<String>) -> Self {
        Self::Audio {
            data: base64_encode(bytes),
            mime_type: mime_type.into(),
        }
    }

    /// A resource link carrying only metadata.
    #[must_use]
    pub fn resource_link(uri: impl Into<String>, name: impl Into<String>) -> Self {
        Self::ResourceLink {
            uri: uri.into(),
            name: name.into(),
            title: None,
            description: None,
            mime_type: None,
            size: None,
        }
    }

    /// An embedded text resource.
    #[must_use]
    pub fn text_resource(
        uri: impl Into<String>,
        mime_type: Option<String>,
        text: impl Into<String>,
    ) -> Self {
        Self::Resource {
            resource: EmbeddedResource {
                uri: uri.into(),
                mime_type,
                text: Some(text.into()),
                blob: None,
            },
        }
    }

    /// An embedded binary resource.
    #[must_use]
    pub fn blob_resource(uri: impl Into<String>, mime_type: Option<String>, bytes: &[u8]) -> Self {
        Self::Resource {
            resource: EmbeddedResource {
                uri: uri.into(),
                mime_type,
                text: None,
                blob: Some(base64_encode(bytes)),
            },
        }
    }

    /// Approximate wire size of this block's payload, for consumers enforcing
    /// their own result budgets.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        match self {
            Self::Image { data, .. } | Self::Audio { data, .. } => data.len(),
            Self::ResourceLink { uri, name, .. } => uri.len() + name.len(),
            Self::Resource { resource } => {
                resource.uri.len()
                    + resource.text.as_ref().map_or(0, String::len)
                    + resource.blob.as_ref().map_or(0, String::len)
            }
        }
    }
}

/// A tool's successful result: the envelope `data` plus extra content blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput<T> {
    pub data: T,
    pub content: Vec<ContentBlock>,
}

impl<T> ToolOutput<T> {
    #[must_use]
    pub const fn new(data: T) -> Self {
        Self {
            data,
            content: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_content(mut self, block: ContentBlock) -> Self {
        self.content.push(block);
        self
    }
}

impl<T> From<T> for ToolOutput<T> {
    fn from(data: T) -> Self {
        Self::new(data)
    }
}

/// Standard base64 with padding (RFC 4648 §4).
#[must_use]
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}
