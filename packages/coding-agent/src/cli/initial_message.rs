//! Build the initial prompt for non-interactive mode.
//!
//! TS reference: `cli/initial-message.ts`. Combines stdin content, `@file`
//! text, and the first positional message into a single string. The TS
//! version also mutates `parsed.messages` by shifting the consumed message
//! off the queue; we mirror that by taking a `&mut Vec<String>` so the
//! caller can iterate the remaining messages afterward.

use model::types::ImageContent;

/// Inputs assembled from CLI parsing, stdin draining, and `@file` expansion.
#[derive(Debug)]
pub struct InitialMessageInput<'a> {
    /// Pending positional messages from the CLI. The first entry, if any,
    /// is consumed and appended to the initial message; the remainder is
    /// left in the vector for the caller to handle (e.g. as follow-up
    /// turns in interactive mode).
    pub messages: &'a mut Vec<String>,
    /// Combined `<file>...</file>` blocks produced by `@file` expansion.
    pub file_text: Option<String>,
    /// Image attachments produced by `@file` expansion.
    pub file_images: Option<Vec<ImageContent>>,
    /// Content piped on stdin, if any.
    pub stdin_content: Option<String>,
}

/// Resulting prompt + image attachments to seed the agent session with.
#[derive(Debug, Default, Clone)]
pub struct InitialMessageResult {
    /// Concatenated prompt: stdin + file_text + first CLI message. `None`
    /// when none of the three sources contributed any content.
    pub initial_message: Option<String>,
    /// Image attachments lifted directly from `file_images`. `None` when
    /// no images were supplied (matches the TS contract that distinguishes
    /// "no images" from "empty list").
    pub initial_images: Option<Vec<ImageContent>>,
}

/// Combine stdin content, `@file` text, and the first CLI message into
/// a single initial prompt for non-interactive mode.
///
/// Mirrors `buildInitialMessage()` in the TS reference. The first entry
/// of `input.messages` is removed (shifted) when present.
pub fn build_initial_message(input: InitialMessageInput<'_>) -> InitialMessageResult {
    let InitialMessageInput {
        messages,
        file_text,
        file_images,
        stdin_content,
    } = input;

    let mut parts: Vec<String> = Vec::new();
    if let Some(stdin) = stdin_content {
        parts.push(stdin);
    }
    if let Some(text) = file_text
        && !text.is_empty()
    {
        parts.push(text);
    }
    if !messages.is_empty() {
        parts.push(messages.remove(0));
    }

    let initial_message = if parts.is_empty() {
        None
    } else {
        Some(parts.join(""))
    };

    let initial_images = match file_images {
        Some(imgs) if !imgs.is_empty() => Some(imgs),
        _ => None,
    };

    InitialMessageResult {
        initial_message,
        initial_images,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_when_all_inputs_empty() {
        let mut messages: Vec<String> = Vec::new();
        let result = build_initial_message(InitialMessageInput {
            messages: &mut messages,
            file_text: None,
            file_images: None,
            stdin_content: None,
        });
        assert!(result.initial_message.is_none());
        assert!(result.initial_images.is_none());
    }

    #[test]
    fn concatenates_stdin_file_text_and_first_message_in_order() {
        let mut messages = vec!["hello".to_string(), "second".to_string()];
        let result = build_initial_message(InitialMessageInput {
            messages: &mut messages,
            file_text: Some("<file>x</file>".to_string()),
            file_images: None,
            stdin_content: Some("piped".to_string()),
        });
        assert_eq!(
            result.initial_message.as_deref(),
            Some("piped<file>x</file>hello")
        );
        // First message consumed; second left for the caller.
        assert_eq!(messages, vec!["second".to_string()]);
    }

    #[test]
    fn preserves_remaining_messages_when_no_stdin_or_files() {
        let mut messages = vec!["only".to_string()];
        let result = build_initial_message(InitialMessageInput {
            messages: &mut messages,
            file_text: None,
            file_images: None,
            stdin_content: None,
        });
        assert_eq!(result.initial_message.as_deref(), Some("only"));
        assert!(messages.is_empty());
    }

    #[test]
    fn empty_file_text_does_not_contribute() {
        let mut messages = vec!["m".to_string()];
        let result = build_initial_message(InitialMessageInput {
            messages: &mut messages,
            file_text: Some(String::new()),
            file_images: None,
            stdin_content: None,
        });
        assert_eq!(result.initial_message.as_deref(), Some("m"));
    }

    #[test]
    fn passes_through_non_empty_images() {
        let mut messages: Vec<String> = Vec::new();
        let img = ImageContent::new("BASE64DATA", "image/png");
        let result = build_initial_message(InitialMessageInput {
            messages: &mut messages,
            file_text: None,
            file_images: Some(vec![img.clone()]),
            stdin_content: None,
        });
        let images = result.initial_images.expect("should yield images");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].data, "BASE64DATA");
        assert_eq!(images[0].mime_type, "image/png");
    }

    #[test]
    fn empty_image_vec_collapses_to_none() {
        let mut messages: Vec<String> = Vec::new();
        let result = build_initial_message(InitialMessageInput {
            messages: &mut messages,
            file_text: None,
            file_images: Some(Vec::new()),
            stdin_content: None,
        });
        assert!(result.initial_images.is_none());
    }

    #[test]
    fn stdin_only_emits_stdin_text() {
        let mut messages: Vec<String> = Vec::new();
        let result = build_initial_message(InitialMessageInput {
            messages: &mut messages,
            file_text: None,
            file_images: None,
            stdin_content: Some("piped-only".to_string()),
        });
        assert_eq!(result.initial_message.as_deref(), Some("piped-only"));
    }
}
