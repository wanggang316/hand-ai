//! Context overflow detection.
//!
//! Detects when a model's context window has been exceeded based on
//! error messages from various providers and silent overflow patterns.

use crate::types::{AssistantMessage, StopReason};

/// Regex-like patterns for context overflow error messages from various providers.
const OVERFLOW_PATTERNS: &[&str] = &[
    "prompt is too long",                  // Anthropic
    "input is too long for requested model", // Amazon Bedrock
    "exceeds the context window",          // OpenAI (Completions & Responses API)
    "input token count",                   // Google (Gemini) - partial, checked with "exceeds the maximum"
    "maximum prompt length is",            // xAI (Grok)
    "reduce the length of the messages",   // Groq
    "maximum context length is",           // OpenRouter
    "exceeds the limit of",               // GitHub Copilot
    "exceeds the available context size",  // llama.cpp server
    "greater than the context length",     // LM Studio
    "context window exceeds limit",        // MiniMax
    "exceeded model token limit",          // Kimi For Coding
    "too large for model with",            // Mistral
    "model_context_window_exceeded",       // z.ai
    "context length exceeded",             // Generic fallback
    "context_length_exceeded",             // Generic fallback (underscore variant)
    "too many tokens",                     // Generic fallback
    "token limit exceeded",                // Generic fallback
];

/// Check if an assistant message represents a context overflow error.
///
/// Handles two cases:
/// 1. **Error-based overflow**: Most providers return `StopReason::Error` with a
///    specific error message pattern.
/// 2. **Silent overflow**: Some providers (e.g., z.ai) accept overflow requests
///    successfully. For these, pass `context_window` to detect when
///    `usage.input > context_window`.
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<u64>) -> bool {
    // Case 1: Check error message patterns
    if message.stop_reason == StopReason::Error
        && let Some(error_msg) = &message.error_message {
            let lower = error_msg.to_lowercase();

            if OVERFLOW_PATTERNS.iter().any(|p| lower.contains(p)) {
                return true;
            }

            // Cerebras returns 400/413 with no body for context overflow
            if (lower.starts_with("400") || lower.starts_with("413"))
                && lower.contains("(no body)")
            {
                return true;
            }
        }

    // Case 2: Silent overflow - successful but usage exceeds context
    if let Some(cw) = context_window
        && message.stop_reason == StopReason::Stop {
            let input_tokens = message.usage.input + message.usage.cache_read;
            if input_tokens > cw {
                return true;
            }
        }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Api, AssistantMessage, Provider, StopReason, Usage};

    fn make_error_message(error_msg: &str) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: Api::AnthropicMessages,
            provider: Provider::Anthropic,
            model: "test".to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::Error,
            error_message: Some(error_msg.to_string()),
            timestamp: 0,
        }
    }

    fn make_ok_message(input_tokens: u64) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: Api::OpenAICompletions,
            provider: Provider::OpenAI,
            model: "test".to_string(),
            usage: Usage {
                input: input_tokens,
                ..Default::default()
            },
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
        }
    }

    #[test]
    fn test_anthropic_overflow() {
        let msg = make_error_message("prompt is too long: 213462 tokens > 200000 maximum");
        assert!(is_context_overflow(&msg, None));
    }

    #[test]
    fn test_openai_overflow() {
        let msg = make_error_message("Your input exceeds the context window of this model");
        assert!(is_context_overflow(&msg, None));
    }

    #[test]
    fn test_google_overflow() {
        let msg = make_error_message(
            "The input token count (1196265) exceeds the maximum number of tokens",
        );
        assert!(is_context_overflow(&msg, None));
    }

    #[test]
    fn test_groq_overflow() {
        let msg = make_error_message("Please reduce the length of the messages or completion");
        assert!(is_context_overflow(&msg, None));
    }

    #[test]
    fn test_cerebras_overflow() {
        let msg = make_error_message("413 status code (no body)");
        assert!(is_context_overflow(&msg, None));
    }

    #[test]
    fn test_mistral_overflow() {
        let msg = make_error_message(
            "Prompt contains 500000 tokens which is too large for model with 200000 maximum context length",
        );
        assert!(is_context_overflow(&msg, None));
    }

    #[test]
    fn test_silent_overflow() {
        let msg = make_ok_message(250_000);
        assert!(is_context_overflow(&msg, Some(200_000)));
    }

    #[test]
    fn test_no_overflow_normal_message() {
        let msg = make_ok_message(50_000);
        assert!(!is_context_overflow(&msg, Some(200_000)));
    }

    #[test]
    fn test_no_overflow_unrelated_error() {
        let msg = make_error_message("Rate limit exceeded");
        assert!(!is_context_overflow(&msg, None));
    }

    #[test]
    fn test_case_insensitive_matching() {
        let msg = make_error_message("PROMPT IS TOO LONG: 213462 tokens > 200000 maximum");
        assert!(is_context_overflow(&msg, None));
    }

    #[test]
    fn test_silent_overflow_with_cache_read() {
        let mut msg = make_ok_message(150_000);
        msg.usage.cache_read = 60_000;
        // Total input = 150k + 60k = 210k > 200k
        assert!(is_context_overflow(&msg, Some(200_000)));
    }
}
