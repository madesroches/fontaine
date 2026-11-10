//! OAuth and headers for streamable HTTP MCP servers, against a scripted wiremock endpoint:
//! a stored token rides the protocol requests as `Authorization: Bearer`, a missing token
//! surfaces the re-login hint, and declared headers (static and helper-produced) reach the
//! server.

mod mcp_common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use fontaine_tools::mcp::{McpErrorKind, ServerSpec, Transport, connect};
use mcp_common::{Gate, names, options, tool_server};
use rmcp::transport::auth::{
    AuthError, CredentialStore, StateStore, StoredAuthorizationState, StoredCredentials,
};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// A credential store scripted by the test.
#[derive(Clone, Default)]
struct MemStore(Arc<Mutex<Option<StoredCredentials>>>);

#[async_trait::async_trait]
impl CredentialStore for MemStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        Ok(self.0.lock().unwrap().clone())
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        *self.0.lock().unwrap() = Some(credentials);
        Ok(())
    }

    async fn clear(&self) -> Result<(), AuthError> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

#[async_trait::async_trait]
impl StateStore for MemStore {
    async fn save(
        &self,
        _csrf_token: &str,
        _state: StoredAuthorizationState,
    ) -> Result<(), AuthError> {
        Ok(())
    }

    async fn load(&self, _csrf_token: &str) -> Result<Option<StoredAuthorizationState>, AuthError> {
        Ok(None)
    }

    async fn delete(&self, _csrf_token: &str) -> Result<(), AuthError> {
        Ok(())
    }
}

fn stored_credentials(token: &str) -> StoredCredentials {
    serde_json::from_value(json!({
        "client_id": "test-client",
        "token_response": {"access_token": token, "token_type": "Bearer"},
    }))
    .expect("a stored-credentials JSON")
}

fn http_spec(url: &str, store: &MemStore) -> ServerSpec {
    ServerSpec {
        name: "remote".to_string(),
        transport: Transport::Http {
            url: url.to_string(),
            headers: BTreeMap::new(),
            headers_helper: None,
            credentials: Arc::new(store.clone()),
            states: Arc::new(store.clone()),
        },
    }
}

/// The streamable-HTTP JSON-RPC double, answering the initialize handshake and a tools/list.
struct JsonRpc;

impl wiremock::Respond for JsonRpc {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap_or_default();
        let id = body["id"].clone();
        let reply = |result: serde_json::Value| {
            ResponseTemplate::new(200)
                .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
        };
        match body["method"].as_str() {
            Some("initialize") => reply(json!({
                "protocolVersion": body["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "web", "version": "0.1.0"},
            })),
            Some("tools/list") => reply(json!({"tools": [{
                "name": "ping",
                "description": "Ping",
                "inputSchema": {"type": "object"},
            }]})),
            _ => ResponseTemplate::new(202),
        }
    }
}

async fn auth_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(JsonRpc)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn a_stored_token_is_sent_on_the_protocol_requests() {
    let server = auth_server().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(JsonRpc)
        .mount(&server)
        .await;

    let store = MemStore::default();
    *store.0.lock().unwrap() = Some(stored_credentials("test-token"));
    let spec = http_spec(&format!("{}/mcp", server.uri()), &store);
    let handle = tool_server();
    let connected = connect(
        &spec,
        handle.clone(),
        &options(Gate::new(true)),
        &Default::default(),
        &|_| None,
    )
    .await
    .expect("the server accepts the token");
    assert_eq!(names(&handle), ["mcp__remote__ping"]);

    let seen = server.received_requests().await.unwrap();
    let authorized = seen
        .iter()
        .filter(|request| {
            request
                .headers
                .get("authorization")
                .is_some_and(|value| value.to_str().unwrap_or_default() == "Bearer test-token")
        })
        .count();
    assert!(authorized > 0, "a protocol request carried the token");
    connected.connection.close().await;
}

#[tokio::test]
async fn no_token_turns_the_401_challenge_into_the_login_hint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(401).insert_header(
            "www-authenticate",
            format!(
                "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
                server.uri()
            ),
        ))
        .mount(&server)
        .await;

    let store = MemStore::default();
    let spec = http_spec(&format!("{}/mcp", server.uri()), &store);
    let error = match connect(
        &spec,
        tool_server(),
        &options(Gate::new(true)),
        &Default::default(),
        &|_| None,
    )
    .await
    {
        Ok(_) => panic!("nobody is logged in"),
        Err(error) => error,
    };
    assert_eq!(error.kind, McpErrorKind::Permanent);
    assert!(
        error.message.contains("fontaine mcp login remote"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn static_and_helper_headers_reach_the_server() {
    let server = auth_server().await;
    let helper = helper_path();

    let store = MemStore::default();
    let spec = ServerSpec {
        name: "remote".to_string(),
        transport: Transport::Http {
            url: format!("{}/mcp", server.uri()),
            headers: BTreeMap::from([
                ("X-Static".to_string(), "declared".to_string()),
                ("X-Both".to_string(), "static-loses".to_string()),
            ]),
            headers_helper: Some(helper),
            credentials: Arc::new(store.clone()),
            states: Arc::new(store.clone()),
        },
    };
    connect(
        &spec,
        tool_server(),
        &options(Gate::new(true)),
        &Default::default(),
        &|_| None,
    )
    .await
    .expect("the endpoint answers");

    let seen = server.received_requests().await.unwrap();
    let with_headers = seen
        .iter()
        .find(|request| request.headers.contains_key("x-static"))
        .expect("a protocol request carried the headers");
    assert_eq!(
        with_headers.headers["x-static"].to_str().unwrap(),
        "declared"
    );
    assert_eq!(
        with_headers.headers["x-helper"].to_str().unwrap(),
        "from-helper"
    );
    // The helper's output overrides a static header of the same name.
    assert_eq!(
        with_headers.headers["x-both"].to_str().unwrap(),
        "helper-wins"
    );
}

/// The built `mcp_header_helper` example next to the test binary.
fn helper_path() -> String {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let file = if cfg!(windows) {
        "mcp_header_helper.exe"
    } else {
        "mcp_header_helper"
    };
    let path = dir.join(file);
    assert!(path.exists(), "{} is not built", path.display());
    path.to_string_lossy().into_owned()
}
