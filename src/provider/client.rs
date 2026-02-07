use anyhow::{Context, Result};
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent,
        ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
        CreateChatCompletionRequest, CreateChatCompletionResponse,
    },
};

/// Wrapper around async-openai's Client with custom base URL support.
#[derive(Clone)]
pub struct LlmClient {
    client: Client<OpenAIConfig>,
}

impl LlmClient {
    /// Create a new client pointing at an OpenAI-compatible API.
    pub fn new(base_url: &str, api_key: &str) -> Self {
        let config = OpenAIConfig::new()
            .with_api_base(base_url)
            .with_api_key(api_key);
        let client = Client::with_config(config);
        Self { client }
    }

    /// Non-streaming chat completion.
    pub async fn chat(
        &self,
        model: &str,
        messages: Vec<ChatCompletionRequestMessage>,
    ) -> Result<CreateChatCompletionResponse> {
        let request = CreateChatCompletionRequest {
            model: model.to_string(),
            messages,
            ..Default::default()
        };

        self.client
            .chat()
            .create(request)
            .await
            .context("chat completion request failed")
    }

    /// Simple convenience: send a user message with an optional system prompt.
    pub async fn simple_chat(
        &self,
        model: &str,
        system_prompt: Option<&str>,
        user_message: &str,
    ) -> Result<String> {
        let mut messages: Vec<ChatCompletionRequestMessage> = Vec::new();

        if let Some(system) = system_prompt {
            messages.push(ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessage {
                    content: ChatCompletionRequestSystemMessageContent::Text(
                        system.to_string(),
                    ),
                    name: None,
                },
            ));
        }

        messages.push(ChatCompletionRequestMessage::User(
            ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(
                    user_message.to_string(),
                ),
                name: None,
            },
        ));

        let response = self.chat(model, messages).await?;

        response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .context("no response content from model")
    }

    /// Get a reference to the underlying async-openai client (for streaming in Phase 4).
    pub fn inner(&self) -> &Client<OpenAIConfig> {
        &self.client
    }
}
