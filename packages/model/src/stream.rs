//! Streaming functions for AI providers.

use crate::api_registry::{get_api_provider, AssistantMessageEventStream};
use crate::types::{
    AssistantMessage, Context, Model, ProviderStreamOptions, SimpleStreamOptions,
};
use futures::StreamExt;

/// Resolve an API provider for the given API type.
fn resolve_api_provider(api: &crate::types::Api) -> crate::api_registry::ApiProviderInternal {
    get_api_provider(api).unwrap_or_else(|| panic!("No API provider registered for api: {:?}", api))
}

/// Stream a response from the model.
pub fn stream(
    model: &Model,
    context: Context,
    options: Option<ProviderStreamOptions>,
) -> AssistantMessageEventStream {
    let provider = resolve_api_provider(&model.api);
    (provider.stream)(model.clone(), context, options)
}

/// Complete a request and return the full message.
pub async fn complete(
    model: &Model,
    context: Context,
    options: Option<ProviderStreamOptions>,
) -> AssistantMessage {
    let mut s = stream(model, context, options);
    
    let mut final_message = None;
    
    while let Some(event) = s.next().await {
        match event {
            crate::types::AssistantMessageEvent::Done { message, .. } => {
                final_message = Some(message);
                break;
            }
            crate::types::AssistantMessageEvent::Error { error, .. } => {
                return error;
            }
            _ => {}
        }
    }
    
    final_message.unwrap_or_else(|| AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: crate::types::Usage::default(),
        stop_reason: crate::types::StopReason::Error,
        error_message: Some("Stream ended without result".to_string()),
        timestamp: current_timestamp_ms(),
    })
}

/// Stream a simple response from the model.
pub fn stream_simple(
    model: &Model,
    context: Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let provider = resolve_api_provider(&model.api);
    (provider.stream_simple)(model.clone(), context, options)
}

/// Complete a simple request and return the full message.
pub async fn complete_simple(
    model: &Model,
    context: Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessage {
    let mut s = stream_simple(model, context, options);
    
    let mut final_message = None;
    
    while let Some(event) = s.next().await {
        match event {
            crate::types::AssistantMessageEvent::Done { message, .. } => {
                final_message = Some(message);
                break;
            }
            crate::types::AssistantMessageEvent::Error { error, .. } => {
                return error;
            }
            _ => {}
        }
    }
    
    final_message.unwrap_or_else(|| AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: crate::types::Usage::default(),
        stop_reason: crate::types::StopReason::Error,
        error_message: Some("Stream ended without result".to_string()),
        timestamp: current_timestamp_ms(),
    })
}

fn current_timestamp_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
