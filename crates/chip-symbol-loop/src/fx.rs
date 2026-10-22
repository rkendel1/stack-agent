//! FX as a pure inference function: one packet in, one reply text out. It holds no task state,
//! runs nothing, reads no repository and decides nothing about completion. Chip validates every
//! reply ([`crate::contract`]). A provider failure is an [`FxFailure`], never a model decision.

use std::sync::Arc;

use async_trait::async_trait;
use chip_inference::{Message, MessageRole, ModelProvider, ModelRequest};
use serde::{Deserialize, Serialize};

use crate::contract::{ClassifyPacket, TaskPacket};

/// One reply as the provider returned it, with the usage it reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxReply {
    pub text: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// The provider did not deliver a reply: infrastructure, not the model's judgment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxFailure {
    pub message: String,
}

#[async_trait]
pub trait Fx: Send + Sync {
    /// The one decision for `packet`.
    async fn decide(&self, packet: &TaskPacket, packet_bytes: &[u8]) -> Result<FxReply, FxFailure>;
    /// The one candidate for `packet`.
    async fn classify(&self, packet: &ClassifyPacket) -> Result<FxReply, FxFailure>;
}

const DECIDE_RULES: &str = "You repair one Rust function. Reply with exactly one JSON object and nothing else. \
Allowed objects (use only ops listed in constraints.allowed_ops):\n\
{\"op\":\"replace_symbol_body\",\"symbol\":\"<symbol.id>\",\"body\":\"{ ... }\"} replaces the body of symbol.id; body is the complete new block, braces included, and nothing else (no signature, no attributes).\n\
{\"op\":\"need_symbol\",\"symbol\":\"<id from neighbors>\"} asks for one neighbor's source.\n\
{\"op\":\"no_change\",\"reason\":\"<text>\"} proposes no edit.\n\
The signature must not change when constraints.signature_frozen is true. You cannot run commands, choose files or declare the task done: the acceptance command decides. Earlier rejected bodies and their errors are in rejected.";

const CLASSIFY_RULES: &str = "Choose the one function most likely responsible for the failure. Reply with exactly one JSON object and nothing else: {\"op\":\"classify\",\"symbol\":\"<one id from candidates>\"}.";

/// FX over an already-configured `chip-inference` provider (a local server, as the experiment
/// requires). The schema hint, when the server enforces one, is configured on the provider itself.
pub struct ModelFx {
    pub provider: Arc<dyn ModelProvider>,
    pub model: String,
    pub max_tokens: u32,
}

impl ModelFx {
    async fn ask(&self, rules: &str, payload: String) -> Result<FxReply, FxFailure> {
        let mut request = ModelRequest::new(
            self.model.clone(),
            vec![
                Message::new(MessageRole::System, rules),
                Message::new(MessageRole::User, payload),
            ],
        );
        request.max_tokens = Some(self.max_tokens);
        request.temperature = Some(0.0);
        match self.provider.complete(request).await {
            Ok(r) => Ok(FxReply {
                text: r.output,
                input_tokens: r.usage.prompt_tokens,
                output_tokens: r.usage.completion_tokens,
            }),
            Err(e) => Err(FxFailure {
                message: e.to_string(),
            }),
        }
    }
}

#[async_trait]
impl Fx for ModelFx {
    async fn decide(
        &self,
        _packet: &TaskPacket,
        packet_bytes: &[u8],
    ) -> Result<FxReply, FxFailure> {
        self.ask(
            DECIDE_RULES,
            String::from_utf8_lossy(packet_bytes).into_owned(),
        )
        .await
    }

    async fn classify(&self, packet: &ClassifyPacket) -> Result<FxReply, FxFailure> {
        let payload = serde_json::to_string(packet).map_err(|e| FxFailure {
            message: e.to_string(),
        })?;
        self.ask(CLASSIFY_RULES, payload).await
    }
}
