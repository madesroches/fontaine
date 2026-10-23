//! OpenCode Go: one subscription serving models on four wire formats behind one gateway. The
//! account signs in once through the Console's device flow, and each request goes to the wire
//! its model is served on, carrying a fresh access token.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use http::header::{HeaderName, HeaderValue};
use rig_core::DynModel;
use rig_core::error::ProviderError;
use rig_core::operation::Completion;
use rig_core::providers::anthropic::{
    Anthropic, AnthropicConfig, Dialect as MessagesDialect, compatible,
};
use rig_core::providers::gemini::{Gemini, GeminiConfig};
use rig_core::providers::openai::wire::Dialect;
use rig_core::providers::openai::{OpenAI, OpenAIConfig, Route};
use serde::Deserialize;
use serde_json::json;

use super::device_flow::{DeviceFlow, DeviceFlowEndpoints, FlowError, RefreshError};
use super::token_source::{Refresher, TokenSource};
use super::{
    AccountClient, Billing, ConnectContext, CredentialPolicy, Provider, SignIn, SignInError,
};
use crate::config::{Account, Wire};
use crate::credentials::{self, OAuthTokens, StoredCredential};
use crate::transport::{AuthPlacement, Decoration, decorated_client, reqwest_client};

pub const DEFAULT_BASE_URL: &str = "https://opencode.ai/inference/go";
pub const CONSOLE_URL: &str = "https://opencode.ai/console";
const CLIENT_ID: &str = "opencode-cli";

/// Never priced: Go bills against subscription allowances. The empty key and URL are
/// placeholders, since the decoration supplies both on every request.
const GO_DIALECT: Dialect = Dialect::gateway("opencode-go", "", "");

/// The Messages models behind Go are not all Anthropic's (Qwen, MiniMax), and the gateway lists
/// none of them in rig's catalog, so the gateway quirks apply: a default `max_tokens` where
/// Anthropic's own dialect would refuse the request, and no strict tool schemas.
const GO_MESSAGES: MessagesDialect = compatible("opencode-go", "", "", None);

pub struct OpencodeGo;

impl Provider for OpencodeGo {
    fn name(&self) -> &'static str {
        "opencode-go"
    }

    fn credential(&self) -> CredentialPolicy {
        CredentialPolicy::SignIn
    }

    fn billing(&self) -> Billing {
        Billing::Unpriced
    }

    fn validate(&self, account: &Account) -> Result<(), (&'static str, String)> {
        if account.api_key_env.is_some() {
            return Err((
                "api_key_env",
                "`opencode-go` accounts sign in with `fontaine auth login <account>`; `api_key_env` is not used"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn accepts_wires(&self) -> bool {
        true
    }

    fn connect(&self, ctx: ConnectContext<'_>) -> Result<Arc<dyn AccountClient>, String> {
        let Some(StoredCredential::OAuth(tokens)) = ctx.credential.clone() else {
            return Err(format!(
                "account `{0}` is not signed in; run `fontaine auth login {0}`",
                ctx.account_name
            ));
        };
        let base = ctx
            .base_url()
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_string();
        let source = TokenSource::shared(
            ctx.data_dir,
            ctx.account_name,
            tokens,
            Arc::new(GoRefresher),
        );
        let mut headers = Vec::new();
        if let Some(id) = ctx.session_id {
            let value = HeaderValue::from_str(id)
                .map_err(|_| "the session id cannot be sent as an HTTP header".to_string())?;
            headers.push((HeaderName::from_static("x-opencode-session"), value));
        }
        let http = |placement: AuthPlacement| {
            decorated_client(Decoration {
                user_agent: Some(fontaine_tools::FONTAINE_USER_AGENT),
                headers: headers.clone(),
                auth: Some((source.clone(), placement)),
            })
        };
        let openai = |route: Option<Route>, root: String| -> Result<OpenAI, String> {
            let mut config = OpenAIConfig::with_key(&GO_DIALECT, "").with_base_url(root);
            if let Some(route) = route {
                config = config.with_route(route);
            }
            Ok(config.connect(http(AuthPlacement::Bearer)?))
        };
        let openai_root = format!("{base}/openai/v1");
        Ok(Arc::new(GoClient {
            chat: openai(Some(Route::Chat), openai_root.clone())?,
            responses: openai(Some(Route::Responses), openai_root)?,
            messages: AnthropicConfig::with_key(&GO_MESSAGES, "")
                .with_base_url(format!("{base}/anthropic"))
                .connect(http(AuthPlacement::Header(HeaderName::from_static(
                    "x-api-key",
                )))?),
            gemini: GeminiConfig::new("")
                .with_base_url(format!("{base}/google"))
                .connect(http(AuthPlacement::GoogleKey)?),
            listing: openai(None, format!("{base}/v1"))?,
            overrides: ctx.account.wires.clone().unwrap_or_default(),
        }))
    }

    fn sign_in(&self) -> Option<&dyn SignIn> {
        Some(self)
    }
}

/// The wire a Go model is served on: the account's override, else the gateway's published
/// model table.
pub fn wire(model: &str, overrides: &BTreeMap<String, Wire>) -> Wire {
    if let Some(wire) = overrides.get(model) {
        return *wire;
    }
    if ["gpt-", "grok-", "muse-spark-"]
        .iter()
        .any(|prefix| model.starts_with(prefix))
    {
        Wire::Responses
    } else if model.starts_with("claude-") || model.starts_with("qwen") || model == "minimax-m3" {
        Wire::Messages
    } else if model.starts_with("gemini-") {
        Wire::Gemini
    } else {
        Wire::Chat
    }
}

struct GoClient {
    chat: OpenAI,
    responses: OpenAI,
    messages: Anthropic,
    gemini: Gemini,
    listing: OpenAI,
    overrides: BTreeMap<String, Wire>,
}

impl AccountClient for GoClient {
    fn completion(&self, model: &str) -> DynModel<Completion> {
        match wire(model, &self.overrides) {
            Wire::Chat => self.chat.completion(model).erase(),
            Wire::Responses => self.responses.completion(model).erase(),
            Wire::Messages => self.messages.completion(model).erase(),
            Wire::Gemini => self.gemini.completion(model).erase(),
        }
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<String>, Box<ProviderError>>> {
        async move {
            let list = self.listing.list_models().await.map_err(Box::new)?;
            Ok(list.data.into_iter().map(|m| m.id).collect())
        }
        .boxed()
    }
}

fn endpoints(server: &str) -> DeviceFlowEndpoints {
    let server = server.trim_end_matches('/');
    DeviceFlowEndpoints {
        code_url: format!("{server}/auth/device/code"),
        token_url: format!("{server}/auth/device/token"),
        client_id: CLIENT_ID,
        code_params: vec![("supports_org_scope", json!(true))],
    }
}

/// Refreshes against the Console the tokens were issued by.
struct GoRefresher;

impl Refresher for GoRefresher {
    fn refresh<'a>(
        &'a self,
        current: &'a OAuthTokens,
    ) -> BoxFuture<'a, Result<OAuthTokens, RefreshError>> {
        async move {
            let server = current
                .extra
                .get("server")
                .map_or(CONSOLE_URL, String::as_str);
            let flow = DeviceFlow::new(endpoints(server))
                .map_err(|e| RefreshError::Failed(FlowError::Invalid(e)))?;
            let mut refreshed = flow.refresh(&current.refresh).await?;
            let mut extra = current.extra.clone();
            extra.append(&mut refreshed.extra);
            refreshed.extra = extra;
            Ok(refreshed)
        }
        .boxed()
    }
}

impl SignIn for OpencodeGo {
    fn sign_in<'a>(
        &'a self,
        account_name: &'a str,
        _account: &'a Account,
        data_dir: &'a Path,
    ) -> BoxFuture<'a, Result<String, SignInError>> {
        sign_in_at(CONSOLE_URL, account_name, data_dir).boxed()
    }
}

#[derive(Deserialize)]
struct UserReply {
    email: Option<String>,
}

/// The device sign-in against the Console at `console`. The URL is a parameter so a test can
/// stand in for the Console; the product only ever passes [`CONSOLE_URL`].
pub async fn sign_in_at(
    console: &str,
    account_name: &str,
    data_dir: &Path,
) -> Result<String, SignInError> {
    let failed = |e: FlowError| SignInError(Box::new(e));
    let console = console.trim_end_matches('/');
    let flow = DeviceFlow::new(endpoints(console)).map_err(|e| failed(FlowError::Invalid(e)))?;
    let code = flow.start().await.map_err(failed)?;
    eprintln!(
        "Open {} and confirm the code {}",
        code.verification_url, code.user_code
    );
    eprintln!("Waiting for approval...");
    let mut tokens = flow.poll(&code).await.map_err(failed)?;
    tokens
        .extra
        .insert("server".to_string(), console.to_string());

    let email = match user_email(console, &tokens.access).await {
        Ok(email) => email,
        Err(e) => {
            eprintln!(
                "fontaine: warning: could not read the account's email: {}",
                fontaine_tools::error_chain::chain(&e)
            );
            None
        }
    };
    if let Some(email) = &email {
        tokens.extra.insert("email".to_string(), email.clone());
    }
    credentials::store_oauth(data_dir, account_name, &tokens)
        .map_err(|e| SignInError(Box::new(e)))?;
    Ok(match email {
        Some(email) => format!("signed in to OpenCode Go as {email}"),
        None => "signed in to OpenCode Go".to_string(),
    })
}

async fn user_email(console: &str, access: &str) -> Result<Option<String>, FlowError> {
    let http = reqwest_client().map_err(FlowError::Invalid)?;
    let response = http
        .get(format!("{console}/api/user"))
        .bearer_auth(access)
        .header("user-agent", fontaine_tools::FONTAINE_USER_AGENT)
        .send()
        .await
        .map_err(|e| FlowError::Request(e.without_url()))?;
    if !response.status().is_success() {
        return Err(FlowError::Invalid(format!(
            "the account lookup answered HTTP {}",
            response.status().as_u16()
        )));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| FlowError::Request(e.without_url()))?;
    let reply: UserReply = serde_json::from_slice(&bytes)
        .map_err(|_| FlowError::Invalid("the account lookup sent no JSON".to_string()))?;
    Ok(reply.email)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(model: &str) -> Wire {
        wire(model, &BTreeMap::new())
    }

    #[test]
    fn the_table_routes_each_family() {
        assert_eq!(route("gpt-5.6-luna"), Wire::Responses);
        assert_eq!(route("grok-5"), Wire::Responses);
        assert_eq!(route("muse-spark-1.3-contributor"), Wire::Responses);
        assert_eq!(route("claude-haiku-5-5"), Wire::Messages);
        assert_eq!(route("qwen3.7-plus"), Wire::Messages);
        assert_eq!(route("minimax-m3"), Wire::Messages);
        assert_eq!(route("minimax-m3.5"), Wire::Chat);
        assert_eq!(route("gemini-4-argon-contributor"), Wire::Gemini);
        assert_eq!(route("kimi-k3"), Wire::Chat);
        assert_eq!(route("glm-5.3-flash"), Wire::Chat);
    }

    #[test]
    fn an_override_beats_the_table() {
        let overrides = BTreeMap::from([("kimi-k3".to_string(), Wire::Messages)]);
        assert_eq!(wire("kimi-k3", &overrides), Wire::Messages);
        assert_eq!(wire("gpt-5", &overrides), Wire::Responses);
    }
}
