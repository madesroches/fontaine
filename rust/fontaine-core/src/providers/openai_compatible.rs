use std::sync::Arc;

use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{Auth, Dialect};

use super::openai::Connected;
use super::{AccountClient, ConnectContext, CredentialPolicy, Provider};
use crate::config::Account;
use crate::transport::caused_client;

/// The one gateway dialect every `openai-compatible` account speaks. Its name is the vendor
/// rig prices by, so it is the kind name and never an account name: such endpoints are unpriced.
const GATEWAY: Dialect = Dialect::gateway("openai-compatible", "", "");

pub struct OpenAiCompatible;

impl Provider for OpenAiCompatible {
    fn name(&self) -> &'static str {
        "openai-compatible"
    }

    fn credential(&self) -> CredentialPolicy {
        CredentialPolicy::Optional
    }

    fn validate(&self, account: &Account) -> Result<(), (&'static str, String)> {
        if account.base_url.as_deref().is_none_or(str::is_empty) {
            return Err((
                "base_url",
                "required for kind = \"openai-compatible\"".to_string(),
            ));
        }
        Ok(())
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let mut config = OpenAIConfig::with_key(&GATEWAY, ctx.api_key().unwrap_or_default())
            .with_auth(Auth::OptionalBearer);
        if let Some(url) = ctx.base_url() {
            config = config.with_base_url(url);
        }
        Ok(Arc::new(Connected(config.connect(caused_client()?))))
    }
}
