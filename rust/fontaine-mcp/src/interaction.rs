//! The one thing an MCP server declares that changes a decision: `requiresUserInteraction`.
//! rig-rmcp drops a tool's `_meta` before the sink sees it, so the marker is read off the wire
//! by a transport wrapper that sees the same `tools/list` responses rig-rmcp registers from.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use fontaine_tools::lock::{read, write};
use rmcp::RoleClient;
use rmcp::model::{ServerJsonRpcMessage, ServerResult, Tool};
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;

pub const REQUIRES_USER_INTERACTION: &str = "anthropic/requiresUserInteraction";

/// The server's own names of the tools any `tools/list` response on this connection marked.
/// It only grows, so no interleaving of overlapping refreshes or late responses can drop a
/// marker that a committed listing carries.
#[derive(Clone, Default)]
pub(crate) struct Marked(Arc<RwLock<HashSet<String>>>);

impl Marked {
    pub(crate) fn contains(&self, tool: &str) -> bool {
        read(&self.0, "MCP interaction markers").contains(tool)
    }

    fn insert(&self, tool: &str) {
        write(&self.0, "MCP interaction markers").insert(tool.to_string());
    }
}

/// Only the JSON Boolean `true` marks; any other value is ignored.
fn marks(tool: &Tool) -> bool {
    tool.meta
        .as_ref()
        .and_then(|meta| meta.get(REQUIRES_USER_INTERACTION))
        .is_some_and(|value| value.as_bool() == Some(true))
}

/// Passes every message through unchanged, noting the tools a `tools/list` response marks.
pub(crate) struct InteractionTap<T> {
    inner: T,
    marked: Marked,
}

impl<T> InteractionTap<T> {
    pub(crate) fn new(inner: T, marked: Marked) -> Self {
        InteractionTap { inner, marked }
    }
}

impl<T: Transport<RoleClient>> Transport<RoleClient> for InteractionTap<T> {
    type Error = T::Error;

    fn name() -> std::borrow::Cow<'static, str> {
        T::name()
    }

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        let message = self.inner.receive().await?;
        if let ServerJsonRpcMessage::Response(response) = &message
            && let ServerResult::ListToolsResult(page) = &response.result
        {
            for tool in page.tools.iter().filter(|tool| marks(tool)) {
                self.marked.insert(&tool.name);
            }
        }
        Some(message)
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;

    use rmcp::model::{
        EmptyResult, JsonRpcResponse, JsonRpcVersion2_0, ListToolsResult, MetaObject, RequestId,
        ServerNotification, ToolListChangedNotification,
    };
    use serde_json::{Value, json};

    use super::*;

    struct Script(VecDeque<ServerJsonRpcMessage>);

    impl Transport<RoleClient> for Script {
        type Error = Infallible;

        // The trait's future must be 'static, which an `async fn` borrowing `self` is not.
        #[allow(clippy::manual_async_fn)]
        fn send(
            &mut self,
            _: TxJsonRpcMessage<RoleClient>,
        ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
            async { Ok(()) }
        }

        async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
            self.0.pop_front()
        }

        async fn close(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn tool(name: &str, marker: Option<Value>) -> Tool {
        let mut tool = Tool::new(name.to_string(), "d", Arc::new(Default::default()));
        tool.meta = marker.map(|value| {
            let mut meta = MetaObject::new();
            meta.insert(REQUIRES_USER_INTERACTION.to_string(), value);
            meta
        });
        tool
    }

    fn page(tools: Vec<Tool>) -> ServerJsonRpcMessage {
        ServerJsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: JsonRpcVersion2_0,
            id: RequestId::Number(1),
            result: ServerResult::ListToolsResult(ListToolsResult::with_all_items(tools)),
        })
    }

    /// Feeds `messages` through a tap, asserting each comes out unchanged.
    async fn tap(messages: Vec<ServerJsonRpcMessage>) -> Marked {
        let marked = Marked::default();
        let mut tap =
            InteractionTap::new(Script(messages.iter().cloned().collect()), marked.clone());
        for expected in messages {
            let got = tap.receive().await.expect("a message");
            assert_eq!(
                serde_json::to_value(got).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        }
        assert!(tap.receive().await.is_none());
        marked
    }

    #[tokio::test]
    async fn only_a_json_true_marks() {
        let marked = tap(vec![page(vec![
            tool("yes", Some(json!(true))),
            tool("string", Some(json!("true"))),
            tool("one", Some(json!(1))),
            tool("no", Some(json!(false))),
            tool("bare", None),
        ])])
        .await;
        assert!(marked.contains("yes"));
        for name in ["string", "one", "no", "bare", "absent"] {
            assert!(!marked.contains(name), "{name}");
        }
    }

    #[tokio::test]
    async fn a_later_unmarked_listing_leaves_the_marker() {
        let marked = tap(vec![
            page(vec![
                tool("a", Some(json!(true))),
                tool("b", Some(json!(true))),
            ]),
            page(vec![tool("a", None)]),
            page(vec![tool("b", Some(json!(false)))]),
        ])
        .await;
        assert!(marked.contains("a"));
        assert!(marked.contains("b"));
    }

    #[tokio::test]
    async fn a_stale_unmarked_response_after_a_marked_one_leaves_the_marker() {
        let marked = tap(vec![
            page(vec![tool("a", Some(json!(true)))]),
            page(vec![tool("a", None)]),
        ])
        .await;
        assert!(marked.contains("a"));
    }

    #[tokio::test]
    async fn other_messages_pass_through_and_mark_nothing() {
        let notification = ServerJsonRpcMessage::Notification(rmcp::model::JsonRpcNotification {
            jsonrpc: JsonRpcVersion2_0,
            notification: ServerNotification::ToolListChangedNotification(
                ToolListChangedNotification::default(),
            ),
        });
        let empty = ServerJsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: JsonRpcVersion2_0,
            id: RequestId::Number(2),
            result: ServerResult::EmptyResult(EmptyResult {}),
        });
        let marked = tap(vec![notification, empty]).await;
        assert!(read(&marked.0, "test").is_empty());
    }
}
