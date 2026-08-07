//! User message and attachment models.
//!
//! Schema-agnostic representation of extracted user content. Normalized
//! attachment output never renders base64; it uses safe placeholders. Image
//! and file attachments are represented by metadata only.

use serde_json::Value;

/// A normalized user message extracted from a transcript record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserMessage {
    /// Terminal-safe, injection-filtered text content.
    pub text: String,
    /// Attachments, represented without any base64 payload.
    pub attachments: Vec<Attachment>,
}

/// A normalized attachment. Base64 data is never retained or rendered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Attachment {
    /// An image attachment. Never contains pixel data.
    Image {
        media_type: Option<String>,
        note: &'static str,
    },
    /// A file attachment. Never contains file contents.
    File {
        filename: Option<String>,
        note: &'static str,
    },
    /// A text content block (e.g., code, tool output) that is safe to display.
    Text { content: String },
}

impl Attachment {
    /// Placeholder used for image attachments instead of base64.
    pub const IMAGE_PLACEHOLDER: &'static str = "[image]";
    /// Placeholder used for file attachments instead of contents.
    pub const FILE_PLACEHOLDER: &'static str = "[file]";

    pub fn image(media_type: Option<String>) -> Self {
        Self::Image {
            media_type,
            note: Self::IMAGE_PLACEHOLDER,
        }
    }

    pub fn file(filename: Option<String>) -> Self {
        Self::File {
            filename,
            note: Self::FILE_PLACEHOLDER,
        }
    }

    /// Render this attachment as a safe display string. Never emits base64.
    pub fn to_display(&self) -> String {
        match self {
            Self::Image { note, media_type } => match media_type {
                Some(mt) => format!("{} ({})", note, mt),
                None => note.to_string(),
            },
            Self::File { note, filename } => match filename {
                Some(name) => format!("{} {}", note, name),
                None => note.to_string(),
            },
            Self::Text { content } => content.clone(),
        }
    }
}

/// Build a [`UserMessage`] from extracted text and attachment list, applying
/// terminal-safe normalization and injection filtering to the text.
pub fn build_user_message(raw_text: Option<String>, attachments: Vec<Attachment>) -> UserMessage {
    use crate::{injection::collapse_known_injections, text};

    let text = raw_text
        .map(|t| {
            let collapsed = collapse_known_injections(&t);
            text::normalize(&collapsed, text::Mode::Normalized)
        })
        .unwrap_or_default();

    UserMessage { text, attachments }
}

/// Attempt to extract a plain-text user string from a heterogeneous JSON value
/// representing message content. Handles string, array of content blocks
/// (text/image/file), and nested objects. Returns `(text, attachments)`.
///
/// This is a best-effort schema-agnostic extractor; integrations may provide
/// their own extraction and call [`build_user_message`] directly.
pub fn extract_content(value: &Value) -> (Option<String>, Vec<Attachment>) {
    match value {
        Value::String(s) => (Some(s.clone()), Vec::new()),
        Value::Array(blocks) => {
            let mut text_parts = Vec::new();
            let mut attachments = Vec::new();
            for block in blocks {
                if let Some(block_obj) = block.as_object() {
                    if let Some(t) = block_obj.get("text").and_then(|v| v.as_str()) {
                        text_parts.push(t.to_string());
                    } else if let Some(t) = block_obj.get("content").and_then(|v| v.as_str()) {
                        text_parts.push(t.to_string());
                    }
                    if let Some(kind) = block_obj.get("type").and_then(|v| v.as_str()) {
                        match kind {
                            "image" => {
                                let media_type = block_obj
                                    .get("media_type")
                                    .or_else(|| block_obj.get("mime_type"))
                                    .and_then(|v| v.as_str())
                                    .map(String::from);
                                attachments.push(Attachment::image(media_type));
                            }
                            "file" => {
                                let filename = block_obj
                                    .get("filename")
                                    .or_else(|| block_obj.get("name"))
                                    .and_then(|v| v.as_str())
                                    .map(String::from);
                                attachments.push(Attachment::file(filename));
                            }
                            _ => {}
                        }
                    }
                }
            }
            let text = if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join("\n"))
            };
            (text, attachments)
        }
        _ => (None, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_only_message_normalizes_text() {
        let msg = build_user_message(Some("hello\nworld".into()), vec![]);
        assert_eq!(msg.text, "hello world");
        assert!(msg.attachments.is_empty());
    }

    #[test]
    fn image_attachment_uses_placeholder_not_base64() {
        let msg = build_user_message(
            Some("see this".into()),
            vec![Attachment::image(Some("image/png".into()))],
        );
        let display = msg.attachments[0].to_display();
        assert!(display.contains("[image]"));
        assert!(!display.contains("base64"));
        assert!(display.contains("image/png"));
    }

    #[test]
    fn file_attachment_uses_placeholder_not_contents() {
        let msg = build_user_message(
            Some("attached".into()),
            vec![Attachment::file(Some("report.pdf".into()))],
        );
        let display = msg.attachments[0].to_display();
        assert!(display.contains("[file]"));
        assert!(display.contains("report.pdf"));
    }

    #[test]
    fn text_attachment_renders_content() {
        let msg = build_user_message(
            None,
            vec![Attachment::Text {
                content: "code snippet".into(),
            }],
        );
        assert_eq!(msg.attachments[0].to_display(), "code snippet");
    }

    #[test]
    fn extract_string_content() {
        let value = serde_json::json!("plain string");
        let (text, attachments) = extract_content(&value);
        assert_eq!(text, Some("plain string".into()));
        assert!(attachments.is_empty());
    }

    #[test]
    fn extract_array_with_text_and_image_blocks() {
        let value = serde_json::json!([
            { "type": "text", "text": "Hello" },
            { "type": "image", "media_type": "image/png", "data": "iVBOR..." }
        ]);
        let (text, attachments) = extract_content(&value);
        assert_eq!(text, Some("Hello".into()));
        assert_eq!(attachments.len(), 1);
        // The base64 data is never stored in the attachment.
        match &attachments[0] {
            Attachment::Image { media_type, .. } => {
                assert_eq!(media_type.as_deref(), Some("image/png"));
            }
            _ => panic!("expected image attachment"),
        }
    }

    #[test]
    fn extract_file_block() {
        let value = serde_json::json!([
            { "type": "file", "filename": "doc.txt", "data": "raw bytes" }
        ]);
        let (_text, attachments) = extract_content(&value);
        assert_eq!(attachments.len(), 1);
        match &attachments[0] {
            Attachment::File { filename, .. } => {
                assert_eq!(filename.as_deref(), Some("doc.txt"));
            }
            _ => panic!("expected file attachment"),
        }
    }

    #[test]
    fn injection_filtering_applied_during_build() {
        let msg = build_user_message(Some("<skill>hidden</skill> visible".into()), vec![]);
        assert_eq!(msg.text, "hidden visible");
    }
}
