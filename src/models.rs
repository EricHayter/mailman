/* API reference:
 * https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create
 *
 * Fields we read from the create request:
 * - "model": not needed yet, but useful if we end up serving multiple models.
 * - "messages": at least one is required. Each has a "content" that is either
 *   a string or a non-empty array of content parts.
 */

use serde::Deserialize;

/// Unknown fields are ignored, so only what we read needs to be listed here.
#[derive(Debug, Deserialize)]
pub struct CreateChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
}

impl CreateChatCompletionRequest {
    /// There must be at least one message, and no empty content part arrays.
    pub fn is_valid(&self) -> bool {
        !self.messages.is_empty()
            && !self.messages.iter().any(|message| {
                matches!(&message.content, Some(Content::ArrayOfContentParts(parts)) if parts.is_empty())
            })
    }
}

#[derive(Debug, Deserialize)]
pub struct Message {
    /// Assistant messages may send null or omit this when they have tool calls.
    pub content: Option<Content>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Content {
    TextContent(String),
    ArrayOfContentParts(Vec<ContentPart>),
}

/// Text parts only: image, audio, and file parts have no `text`, so they fail
/// to deserialize and the request is rejected.
#[derive(Debug, Deserialize)]
pub struct ContentPart {
    pub text: String,
}
