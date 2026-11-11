//! The four rig clients of an account that serves several wires, and the dispatch from a wire
//! to the model it builds. Shared by every kind that picks a wire per model.

use rig_core::DynModel;
use rig_core::catalog::ModelFacts;
use rig_core::operation::Completion;
use rig_core::providers::anthropic::{Anthropic, ToolInputStreaming};
use rig_core::providers::gemini::Gemini;
use rig_core::providers::openai::OpenAI;

use super::{CacheScope, PromptCaching};
use crate::config::Wire;

pub(crate) struct WireClients {
    pub chat: OpenAI,
    pub responses: OpenAI,
    pub messages: Anthropic,
    pub gemini: Gemini,
    pub caching: PromptCaching,
}

impl WireClients {
    /// `model` on `wire`, with `facts` bound and the account's cache breakpoints on Messages.
    /// `tool_input` replaces the Messages wire's tool-input streaming mode when set.
    pub(crate) fn completion(
        &self,
        wire: Wire,
        model: &str,
        facts: Option<ModelFacts>,
        scope: CacheScope,
        tool_input: Option<ToolInputStreaming>,
    ) -> DynModel<Completion> {
        macro_rules! bound {
            ($built:expr) => {{
                let mut built = $built;
                if let Some(facts) = facts {
                    built.wire = built.wire.with_facts(facts);
                }
                built
            }};
        }
        match wire {
            Wire::Chat => bound!(self.chat.completion(model)).erase(),
            Wire::Responses => bound!(self.responses.completion(model)).erase(),
            Wire::Messages => {
                let mut built = bound!(self.messages.completion(model));
                if let Some(mode) = tool_input {
                    built.wire = built.wire.with_tool_input_streaming(mode);
                }
                self.caching.apply(built, scope).erase()
            }
            Wire::Gemini => bound!(self.gemini.completion(model)).erase(),
        }
    }
}
