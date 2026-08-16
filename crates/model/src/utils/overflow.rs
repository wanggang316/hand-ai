//! Context overflow detection.
//!
//! Detects when a model's context window has been exceeded based on
//! error messages from various providers and silent overflow patterns.

use crate::types::{AssistantMessage, StopReason};

/// Regex-like patterns for context overflow error messages from various providers.
const OVERFLOW_PATTERNS: &[&str] = &[
    "prompt is too long",                    // Anthropic token-count overflow
    "request_too_large",                     // Anthropic HTTP 413 byte-size overflow
    "input is too long for requested model", // Amazon Bedrock
    "exceeds the context window",            // OpenAI (Completions & Responses API)
    "input token count", // Google (Gemini) - partial, checked with "exceeds the maximum"
    "maximum prompt length is", // xAI (Grok)
    "reduce the length of the messages", // Groq
    "maximum context length is", // OpenRouter
    "exceeds the limit of", // GitHub Copilot
    "exceeds the available context size", // llama.cpp server
    "greater than the context length", // LM Studio
    "context window exceeds limit", // MiniMax
    "exceeded model token limit", // Kimi For Coding
    "too large for model with", // Mistral
    "model_context_window_exceeded", // z.ai
    "prompt too long; exceeded", // Ollama explicit overflow error
    "context length exceeded", // Generic fallback
    "context_length_exceeded", // Generic fallback (underscore variant)
    "too many tokens",   // Generic fallback
    "token limit exceeded", // Generic fallback
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
        && let Some(error_msg) = &message.error_message
    {
        let lower = error_msg.to_lowercase();

        // Skip messages that look like throttling / rate-limit errors
        // even though they share token-related vocabulary with real
        // overflow. AWS Bedrock formats throttling as
        // `ThrottlingException: Too many tokens, please wait ...`
        // which would otherwise trip the `too many tokens` overflow
        // pattern. 429s and generic rate-limit strings get the same
        // treatment — they're transient, not context-overflow.
        if is_non_overflow_error(&lower) {
            return false;
        }

        if OVERFLOW_PATTERNS.iter().any(|p| lower.contains(p)) {
            return true;
        }

        // Cerebras returns 400/413 with no body for context overflow
        if (lower.starts_with("400") || lower.starts_with("413")) && lower.contains("(no body)") {
            return true;
        }
    }

    // (Falls through to silent-overflow check.)
    // Case 2: Silent overflow - successful but usage exceeds context
    if let Some(cw) = context_window
        && message.stop_reason == StopReason::Stop
    {
        let input_tokens = message.usage.input + message.usage.cache_read;
        if input_tokens > cw {
            return true;
        }
    }

    // Case 3: Length-stop overflow (Xiaomi MiMo style). The server
    // truncates the oversized input to exactly fill the context window,
    // leaving no room to generate, then closes the stream with
    // `finish_reason = "length"` and `completion_tokens = 0`. Detect
    // the signal: stop reason `length`, zero output, and input + cache
    // hits filling >=99% of the context window (use 99% to tolerate
    // off-by-a-few token rounding the server applies).
    if let Some(cw) = context_window
        && message.stop_reason == StopReason::Length
        && message.usage.output == 0
    {
        let input_tokens = message.usage.input + message.usage.cache_read;
        // `cw * 99 / 100` (integer-safe) instead of casting to f64.
        let threshold = (cw.saturating_mul(99)) / 100;
        if input_tokens >= threshold {
            return true;
        }
    }

    false
}

/// Returns true when the (already-lowercased) error message looks like
/// a throttling / rate-limit error rather than a real context
/// overflow. These share some vocabulary with overflow patterns
/// (notably "too many tokens") and must be excluded so the agent
/// loop treats them as retryable rate-limit errors, not as overflow
/// that needs context trimming.
fn is_non_overflow_error(lower: &str) -> bool {
    lower.starts_with("throttling")
        || lower.starts_with("service unavailable:")
        || lower.contains("throttlingexception")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
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
            raw_stop_reason: None,
            error_message: Some(error_msg.to_string()),
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
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
            raw_stop_reason: None,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    #[test]
    fn test_anthropic_overflow() {
        let msg = make_error_message("prompt is too long: 213462 tokens > 200000 maximum");
        assert!(is_context_overflow(&msg, None));
    }

    /// AWS Bedrock formats throttling as
    /// `ThrottlingException: Too many tokens, please wait ...`. The
    /// "too many tokens" tail matches the generic overflow pattern
    /// but the actual cause is rate limiting, not context overflow.
    /// Misclassifying a throttle as overflow would silently trim the
    /// transcript and re-send instead of backing off — wasted work
    /// at best, data loss at worst. Pin the exclusion explicitly.
    #[test]
    fn test_bedrock_throttling_is_not_overflow() {
        let msg = make_error_message(
            "ThrottlingException: Too many tokens, please wait before trying again.",
        );
        assert!(
            !is_context_overflow(&msg, None),
            "throttling must NOT be classified as overflow"
        );
    }

    /// Generic HTTP 429 / rate-limit strings get the same treatment —
    /// they are transient errors, not overflow.
    #[test]
    fn test_rate_limit_errors_are_not_overflow() {
        for raw in [
            "HTTP 429 too many requests: please retry after 30s",
            "rate limit exceeded for this organization",
        ] {
            let msg = make_error_message(raw);
            assert!(
                !is_context_overflow(&msg, None),
                "rate limit '{raw}' must NOT be classified as overflow"
            );
        }
    }

    /// Ollama deployments behave differently around context overflow.
    /// Many setups truncate the input silently (undetectable here
    /// because we don't know the expected token count), but some
    /// return an explicit error string like
    /// `prompt too long; exceeded max context length by N tokens`.
    /// Match the explicit error so callers can trim and retry.
    #[test]
    fn test_ollama_overflow() {
        let msg = make_error_message("prompt too long; exceeded max context length by 1024 tokens");
        assert!(is_context_overflow(&msg, None));
        // Some Ollama builds drop the "max " qualifier.
        let msg2 = make_error_message("prompt too long; exceeded context length");
        assert!(is_context_overflow(&msg2, None));
    }

    /// Anthropic returns HTTP 413 with a `request_too_large` error code
    /// when the WIRE size of the request (after caching / image
    /// expansion) exceeds the per-request byte cap, even when the
    /// token count is under the model's context window. This is a
    /// separate overflow path from the token-count "prompt is too
    /// long" message and must be classified as a context-overflow
    /// error so callers can trim and retry.
    #[test]
    fn test_anthropic_request_too_large_overflow() {
        let msg = make_error_message(
            "HTTP 413: {\"error\":{\"type\":\"request_too_large\",\"message\":\"Request exceeds the maximum size\"}}",
        );
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

    fn make_length_stop_message(input_tokens: u64) -> AssistantMessage {
        AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: Api::OpenAICompletions,
            provider: Provider::OpenAI,
            model: "mimo-v2.5-pro".to_string(),
            usage: Usage {
                input: input_tokens,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                total_tokens: input_tokens,
                cost: Default::default(),
            },
            stop_reason: StopReason::Length,
            raw_stop_reason: None,
            error_message: None,
            timestamp: 0,
            response_model: None,
            response_id: None,
            diagnostics: None,
        }
    }

    /// Length-stop overflow: providers like Xiaomi MiMo truncate the
    /// oversized input to exactly fill the context window, then close
    /// the stream with `finish_reason = "length"` and zero output.
    /// Detect it when input+cache_read fills >=99% of the context window.
    #[test]
    fn test_length_stop_overflow_at_context_window() {
        let msg = make_length_stop_message(200_000);
        assert!(is_context_overflow(&msg, Some(200_000)));
    }

    /// 99% threshold tolerates a small rounding slack the server may
    /// apply when truncating.
    #[test]
    fn test_length_stop_overflow_within_one_percent_slack() {
        let msg = make_length_stop_message(199_000);
        assert!(is_context_overflow(&msg, Some(200_000)));
    }

    /// Below the 99% threshold the length stop is a normal completion
    /// hitting `max_tokens`, not a context overflow.
    #[test]
    fn test_length_stop_not_overflow_when_below_threshold() {
        let msg = make_length_stop_message(150_000);
        assert!(!is_context_overflow(&msg, Some(200_000)));
    }

    /// Length-stop with non-zero output is a normal `max_tokens` cutoff,
    /// not overflow — the model had room to generate.
    #[test]
    fn test_length_stop_with_output_not_overflow() {
        let mut msg = make_length_stop_message(200_000);
        msg.usage.output = 4_096;
        assert!(!is_context_overflow(&msg, Some(200_000)));
    }

    /// Length-stop signal needs a context window — without it we can
    /// only treat the stop reason as a normal `max_tokens` cutoff.
    #[test]
    fn test_length_stop_requires_context_window() {
        let msg = make_length_stop_message(200_000);
        assert!(!is_context_overflow(&msg, None));
    }
}
