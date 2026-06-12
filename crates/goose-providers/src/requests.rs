use std::{collections::HashMap, error::Error, fmt::Display};

use rmcp::model::Tool;
use serde_json::Value;

use crate::{conversation::message::Message, images::ImageFormat, thinking::ThinkingEffort};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidRequestError {
    message: String,
}

impl InvalidRequestError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for InvalidRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid request: {}", self.message)
    }
}

impl Error for InvalidRequestError {}

impl From<String> for InvalidRequestError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for InvalidRequestError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

pub struct RequestParts<'a> {
    pub model_name: &'a str,
    pub system: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [Tool],
    pub image_format: &'a ImageFormat,
    pub for_streaming: bool,
    pub thinking_effort: Option<ThinkingEffort>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<i32>,
    pub request_params: Option<&'a HashMap<String, Value>>,
}

pub trait ProviderRequestBuilder {
    fn build_request(&self, request: &RequestParts<'_>) -> Result<Value, InvalidRequestError>;
}

pub struct RequestBuilder<'a, P> {
    parts: RequestParts<'a>,
    provider: P,
}

impl<'a, P> RequestBuilder<'a, P> {
    pub fn new(
        model_name: &'a str,
        system: &'a str,
        messages: &'a [Message],
        tools: &'a [Tool],
        image_format: &'a ImageFormat,
        provider: P,
    ) -> Self {
        Self {
            parts: RequestParts {
                model_name,
                system,
                messages,
                tools,
                image_format,
                for_streaming: false,
                thinking_effort: None,
                temperature: None,
                max_tokens: None,
                request_params: None,
            },
            provider,
        }
    }

    pub fn with_streaming(mut self, for_streaming: bool) -> Self {
        self.parts.for_streaming = for_streaming;
        self
    }

    pub fn with_thinking_effort(mut self, thinking_effort: Option<ThinkingEffort>) -> Self {
        self.parts.thinking_effort = thinking_effort;
        self
    }

    pub fn with_temperature(mut self, temperature: Option<f32>) -> Self {
        self.parts.temperature = temperature;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: Option<i32>) -> Self {
        self.parts.max_tokens = max_tokens;
        self
    }

    pub fn with_request_params(
        mut self,
        request_params: Option<&'a HashMap<String, Value>>,
    ) -> Self {
        self.parts.request_params = request_params;
        self
    }

    pub fn parts(&self) -> &RequestParts<'a> {
        &self.parts
    }

    pub(crate) fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }
}

impl<P> RequestBuilder<'_, P>
where
    P: ProviderRequestBuilder,
{
    pub fn build(&self) -> Result<Value, InvalidRequestError> {
        self.provider.build_request(self.parts())
    }
}
