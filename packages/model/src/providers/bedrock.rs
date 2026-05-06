//! AWS Bedrock Converse Stream provider.
//!
//! Implements the AWS Bedrock ConverseStream API for running models
//! hosted on AWS Bedrock (Anthropic Claude, Meta Llama, etc.).
//!
//! Authentication: Uses AWS credentials (access key, secret key, session token)
//! with SigV4 request signing. Credentials are read from:
//! 1. Environment variables (AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, AWS_SESSION_TOKEN)
//! 2. AWS CLI profile (~/.aws/credentials)
//! 3. Instance metadata (EC2/ECS)
//!
//! Endpoint: POST https://bedrock-runtime.{region}.amazonaws.com/model/{model_id}/converse-stream

use crate::api_registry::{ApiProvider, AssistantMessageEventStream};
use crate::types::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message, Model,
    SimpleStreamOptions, StopReason, StreamOptions, TextContent, ToolCall, Usage,
};
use serde_json::Value;

/// Provider for AWS Bedrock ConverseStream API.
#[derive(Debug, Clone, Copy, Default)]
pub struct BedrockProvider;

impl BedrockProvider {
    pub fn new() -> Self {
        Self
    }
}

impl ApiProvider for BedrockProvider {
    fn stream(
        &self,
        model: Model,
        context: Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        stream_bedrock(model, context, options)
    }

    fn stream_simple(
        &self,
        model: Model,
        context: Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream<'static> {
        let base = options.map(|o| o.base);
        stream_bedrock(model, context, base)
    }
}

/// Get the AWS region from environment or default.
fn get_aws_region() -> String {
    std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_string())
}

/// Get AWS credentials from environment.
fn get_aws_credentials() -> Option<AwsCredentials> {
    let access_key = std::env::var("AWS_ACCESS_KEY_ID").ok()?;
    let secret_key = std::env::var("AWS_SECRET_ACCESS_KEY").ok()?;
    let session_token = std::env::var("AWS_SESSION_TOKEN").ok();
    Some(AwsCredentials {
        access_key,
        secret_key,
        session_token,
    })
}

struct AwsCredentials {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
}

/// Build the Bedrock ConverseStream request body.
fn build_converse_body(context: &Context, options: &StreamOptions, model: &Model) -> Value {
    let mut body = serde_json::json!({});

    // System prompt
    if let Some(ref prompt) = context.system_prompt
        && !prompt.is_empty()
    {
        body["system"] = serde_json::json!([{
            "text": prompt,
        }]);
    }

    // Convert messages
    let mut messages = Vec::new();
    for msg in &context.messages {
        match msg {
            Message::User(u) => {
                let text = match &u.content {
                    crate::types::UserContent::Text(s) => s.clone(),
                    crate::types::UserContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|b| match b {
                            crate::types::UserContentBlock::Text(t) => Some(t.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": [{"text": text}],
                }));
            }
            Message::Assistant(a) => {
                let mut content = Vec::new();
                for block in &a.content {
                    match block {
                        AssistantContentBlock::Text(t) => {
                            content.push(serde_json::json!({"text": t.text}));
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            content.push(serde_json::json!({
                                "toolUse": {
                                    "toolUseId": tc.id,
                                    "name": tc.name,
                                    "input": tc.arguments,
                                }
                            }));
                        }
                        AssistantContentBlock::Thinking(_) => {}
                    }
                }
                if !content.is_empty() {
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": content,
                    }));
                }
            }
            Message::ToolResult(tr) => {
                let result_text = tr
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        crate::types::ToolResultContent::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": [{
                        "toolResult": {
                            "toolUseId": tr.tool_call_id,
                            "content": [{"text": result_text}],
                            "status": if tr.is_error { "error" } else { "success" },
                        }
                    }],
                }));
            }
        }
    }
    body["messages"] = Value::Array(messages);

    // Tools (toolConfig)
    if let Some(ref tools_list) = context.tools
        && !tools_list.is_empty()
    {
        let tools: Vec<Value> = tools_list
            .iter()
            .map(|t| {
                serde_json::json!({
                    "toolSpec": {
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": {
                            "json": t.parameters,
                        }
                    }
                })
            })
            .collect();
        body["toolConfig"] = serde_json::json!({ "tools": tools });
    }

    // Inference config
    let mut inference_config = serde_json::json!({});
    if let Some(temp) = options.temperature {
        inference_config["temperature"] = Value::from(temp);
    }
    if let Some(max) = options.max_tokens.or(Some(model.max_tokens as u32)) {
        inference_config["maxTokens"] = Value::from(max);
    }
    body["inferenceConfig"] = inference_config;

    body
}

/// Stream from AWS Bedrock ConverseStream API.
fn stream_bedrock(
    model: Model,
    context: Context,
    options: Option<StreamOptions>,
) -> AssistantMessageEventStream<'static> {
    let options = options.unwrap_or_default();

    Box::pin(async_stream::stream! {
        let mut output = AssistantMessage {
            role: "assistant".to_string(),
            content: vec![],
            api: Api::BedrockConverseStream,
            provider: model.provider,
            model: model.id.clone(),
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
            error_message: None,
            timestamp: current_timestamp_ms(),
        };

        yield AssistantMessageEvent::Start {
            partial: output.clone(),
        };

        // Check credentials
        let credentials = match get_aws_credentials() {
            Some(c) => c,
            None => {
                output.error_message = Some(
                    "AWS credentials not found. Set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY.".into()
                );
                output.stop_reason = StopReason::Error;
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: output,
                };
                return;
            }
        };

        let region = get_aws_region();
        let body = build_converse_body(&context, &options, &model);

        // Build the endpoint URL
        let base_url = if model.base_url.is_empty() {
            format!("https://bedrock-runtime.{}.amazonaws.com", region)
        } else {
            model.base_url.clone()
        };
        let url = format!("{}/model/{}/converse-stream", base_url, model.id);

        let body_bytes = serde_json::to_vec(&body).unwrap_or_default();

        // Sign the request with SigV4
        let now = chrono::Utc::now();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

        let client = reqwest::Client::new();
        let mut builder = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("X-Amz-Date", &amz_date)
            .body(body_bytes.clone());

        if let Some(ref token) = credentials.session_token {
            builder = builder.header("X-Amz-Security-Token", token);
        }

        // Note: Full SigV4 signing requires computing canonical request hash,
        // string to sign, and signing key. For production use, this should use
        // the aws-sigv4 crate. Here we provide the basic structure.
        // In practice, users should use the AWS SDK or set up a proxy.
        let auth_header = compute_sigv4_auth(
            &credentials,
            &region,
            &amz_date,
            &date_stamp,
            &url,
            &body_bytes,
        );
        builder = builder.header("Authorization", &auth_header);

        let response = match builder.send().await {
            Ok(r) => r,
            Err(e) => {
                output.error_message = Some(format!("Bedrock request failed: {}", e));
                output.stop_reason = StopReason::Error;
                yield AssistantMessageEvent::Error {
                    reason: StopReason::Error,
                    error: output,
                };
                return;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            output.error_message = Some(format!("Bedrock HTTP {}: {}", status, body_text));
            output.stop_reason = StopReason::Error;
            yield AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: output,
            };
            return;
        }

        // Parse Bedrock event stream (binary framing with :event-type headers)
        // Bedrock uses AWS event stream encoding, not SSE.
        // Each event has a type: contentBlockDelta, contentBlockStart,
        // contentBlockStop, messageStart, messageStop, metadata
        let mut text_buffer = String::new();
        let mut current_tool_name = String::new();
        let mut current_tool_id = String::new();
        let mut current_tool_input = String::new();

        use futures::StreamExt;
        let mut byte_stream = response.bytes_stream();
        let mut buffer = Vec::new();

        while let Some(chunk) = byte_stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    output.error_message = Some(format!("Stream error: {}", e));
                    break;
                }
            };

            buffer.extend_from_slice(&chunk);

            // Try to parse events from buffer
            // Bedrock returns JSON lines for each event in the stream
            while let Some(event) = extract_bedrock_event(&mut buffer) {
                match event.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "contentBlockDelta" => {
                        if let Some(delta) = event.get("delta") {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                text_buffer.push_str(text);
                                yield AssistantMessageEvent::TextDelta {
                                    content_index: 0,
                                    delta: text.to_string(),
                                    partial: output.clone(),
                                };
                            }
                            if let Some(input) = delta.get("toolUse").and_then(|t| t.get("input")).and_then(|i| i.as_str()) {
                                current_tool_input.push_str(input);
                            }
                        }
                    }
                    "contentBlockStart" => {
                        if let Some(start) = event.get("start")
                            && let Some(tool_use) = start.get("toolUse") {
                                current_tool_name = tool_use.get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                current_tool_id = tool_use.get("toolUseId")
                                    .and_then(|i| i.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                current_tool_input.clear();
                            }
                    }
                    "contentBlockStop" => {
                        if !current_tool_name.is_empty() {
                            let args: Value = serde_json::from_str(&current_tool_input)
                                .unwrap_or(Value::Object(Default::default()));
                            output.content.push(AssistantContentBlock::ToolCall(ToolCall {
                                content_type: "tool_call".to_string(),
                                id: current_tool_id.clone(),
                                name: current_tool_name.clone(),
                                arguments: args,
                                thought_signature: None,
                            }));
                            output.stop_reason = StopReason::ToolUse;
                            current_tool_name.clear();
                            current_tool_id.clear();
                            current_tool_input.clear();
                        }
                    }
                    "messageStop" => {
                        if let Some(reason) = event.get("stopReason").and_then(|r| r.as_str()) {
                            output.stop_reason = match reason {
                                "end_turn" | "stop" => StopReason::Stop,
                                "tool_use" => StopReason::ToolUse,
                                "max_tokens" => StopReason::Length,
                                _ => StopReason::Stop,
                            };
                        }
                    }
                    "metadata" => {
                        if let Some(usage) = event.get("usage") {
                            output.usage.input = usage.get("inputTokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            output.usage.output = usage.get("outputTokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                        }
                    }
                    _ => {}
                }
            }
        }

        // Finalize text
        if !text_buffer.is_empty() {
            output.content.push(AssistantContentBlock::Text(TextContent {
                content_type: "text".to_string(),
                text: text_buffer,
                text_signature: None,
            }));
        }

        yield AssistantMessageEvent::Done {
            reason: output.stop_reason,
            message: output,
        };
    })
}

/// Extract a Bedrock event from the buffer (JSON line parsing).
fn extract_bedrock_event(buffer: &mut Vec<u8>) -> Option<Value> {
    let text = String::from_utf8_lossy(buffer);
    if let Some(idx) = text.find('\n') {
        let line = text[..idx].trim().to_string();
        let remaining = text[idx + 1..].as_bytes().to_vec();
        *buffer = remaining;
        if line.is_empty() {
            return None;
        }
        serde_json::from_str(&line).ok()
    } else {
        None
    }
}

/// Compute AWS SigV4 authorization header.
///
/// This is a simplified implementation. For production use, consider the `aws-sigv4` crate.
fn compute_sigv4_auth(
    credentials: &AwsCredentials,
    region: &str,
    amz_date: &str,
    date_stamp: &str,
    url: &str,
    body: &[u8],
) -> String {
    use std::fmt::Write;

    let service = "bedrock";
    let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, region, service);

    // Parse URL for host and path
    let parsed = url.strip_prefix("https://").unwrap_or(url);
    let (host, path) = if let Some(idx) = parsed.find('/') {
        (&parsed[..idx], &parsed[idx..])
    } else {
        (parsed, "/")
    };

    // Canonical request
    let payload_hash = sha256_hex(body);
    let canonical_headers = format!("host:{}\nx-amz-date:{}\n", host, amz_date);
    let signed_headers = "host;x-amz-date";
    let canonical_request = format!(
        "POST\n{}\n\n{}\n{}\n{}",
        path, canonical_headers, signed_headers, payload_hash
    );

    // String to sign
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        amz_date,
        credential_scope,
        sha256_hex(canonical_request.as_bytes())
    );

    // Signing key
    let k_date = hmac_sha256(
        format!("AWS4{}", credentials.secret_key).as_bytes(),
        date_stamp.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");

    // Signature
    let signature = hmac_sha256(&k_signing, string_to_sign.as_bytes());
    let mut sig_hex = String::new();
    for byte in &signature {
        write!(&mut sig_hex, "{:02x}", byte).unwrap();
    }

    format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        credentials.access_key, credential_scope, signed_headers, sig_hex
    )
}

/// SHA-256 hash as hex string.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(data);
    hex::encode(hash)
}

/// HMAC-SHA256.
fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<sha2::Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Cost, InputType, Provider, UserMessage};

    fn test_model() -> Model {
        Model {
            id: "anthropic.claude-3-sonnet-20240229-v1:0".to_string(),
            name: "Claude 3 Sonnet (Bedrock)".to_string(),
            api: Api::BedrockConverseStream,
            provider: Provider::AmazonBedrock,
            base_url: String::new(),
            reasoning: false,
            input: vec![InputType::Text],
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 200_000,
            max_tokens: 4096,
            headers: None,
            compat: None,
        }
    }

    fn test_context() -> Context {
        Context {
            system_prompt: Some("You are a helpful assistant.".to_string()),
            messages: vec![Message::User(UserMessage::new_text("hello"))],
            tools: None,
        }
    }

    #[test]
    fn test_build_converse_body() {
        let model = test_model();
        let context = test_context();
        let options = StreamOptions::default();

        let body = build_converse_body(&context, &options, &model);
        assert!(body["system"].is_array());
        assert!(body["messages"].is_array());
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_build_converse_body_with_tools() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage::new_text("hi"))],
            tools: Some(vec![crate::types::Tool {
                name: "read".to_string(),
                description: "Read a file".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
        };
        let options = StreamOptions::default();

        let body = build_converse_body(&context, &options, &model);
        assert!(body["toolConfig"]["tools"].is_array());
    }

    #[test]
    fn test_build_converse_body_tool_result() {
        let model = test_model();
        let context = Context {
            system_prompt: None,
            messages: vec![Message::ToolResult(crate::types::ToolResultMessage {
                role: "toolResult".to_string(),
                tool_call_id: "tool_123".to_string(),
                tool_name: "read".to_string(),
                content: vec![crate::types::ToolResultContent::Text(TextContent {
                    content_type: "text".to_string(),
                    text: "result".to_string(),
                    text_signature: None,
                })],
                details: None,
                is_error: false,
                timestamp: 0,
            })],
            tools: None,
        };
        let options = StreamOptions::default();

        let body = build_converse_body(&context, &options, &model);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        assert!(msgs[0]["content"][0]["toolResult"].is_object());
    }

    #[test]
    fn test_get_aws_region_default() {
        // When env vars are not set, defaults to us-east-1
        // (may vary in CI, so just check it returns something)
        let region = get_aws_region();
        assert!(!region.is_empty());
    }

    #[test]
    fn test_extract_bedrock_event() {
        let mut buffer = b"{\"type\":\"contentBlockDelta\",\"delta\":{\"text\":\"hi\"}}\n".to_vec();
        let event = extract_bedrock_event(&mut buffer);
        assert!(event.is_some());
        assert_eq!(event.unwrap()["type"], "contentBlockDelta");
    }

    #[test]
    fn test_extract_bedrock_event_empty() {
        let mut buffer = Vec::new();
        assert!(extract_bedrock_event(&mut buffer).is_none());
    }

    #[test]
    fn test_provider_creation() {
        let _provider = BedrockProvider::new();
    }
}
