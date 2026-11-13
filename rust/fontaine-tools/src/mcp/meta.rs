//! What a server declares in a tool's `_meta`: `requiresUserInteraction`, the one value that
//! changes a decision, and `maxResultSizeChars`, which only sizes a buffer. rig-rmcp drops a
//! tool's `_meta` before the sink sees it, so both are read off the wire by a transport wrapper
//! that sees the same `tools/list` responses rig-rmcp registers from.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use super::output::{DEFAULT_CHAR_LIMIT, MAX_CHAR_LIMIT};
use crate::lock::{read, write};
use rmcp::RoleClient;
use rmcp::model::{
    JsonRpcNotification, JsonRpcVersion2_0, ServerJsonRpcMessage, ServerNotification, ServerResult,
    Tool, ToolListChangedNotification,
};
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use tokio::sync::Notify;

pub const REQUIRES_USER_INTERACTION: &str = "anthropic/requiresUserInteraction";
pub const MAX_RESULT_SIZE_CHARS: &str = "anthropic/maxResultSizeChars";

#[derive(Default)]
struct Facts {
    /// The server's own names of the tools any `tools/list` response marked. It only grows, so no
    /// interleaving of overlapping refreshes or late responses can drop a marker that a
    /// committed listing carries.
    marked: HashSet<String>,
    /// The latest listing wins here: the value only sizes a buffer and decides nothing.
    char_limits: HashMap<String, usize>,
}

/// What this connection's `tools/list` responses declared about each tool.
#[derive(Clone, Default)]
pub(crate) struct ToolMeta(Arc<RwLock<Facts>>);

impl ToolMeta {
    pub(crate) fn requires_user_interaction(&self, tool: &str) -> bool {
        read(&self.0, "MCP tool metadata").marked.contains(tool)
    }

    /// The most characters of `tool`'s successful result kept in context.
    pub(crate) fn char_limit(&self, tool: &str) -> usize {
        read(&self.0, "MCP tool metadata")
            .char_limits
            .get(tool)
            .copied()
            .unwrap_or(DEFAULT_CHAR_LIMIT)
    }

    fn record(&self, tool: &Tool) {
        let mut facts = write(&self.0, "MCP tool metadata");
        if marks(tool) {
            facts.marked.insert(tool.name.to_string());
        }
        match declared_char_limit(tool) {
            Some(limit) => facts.char_limits.insert(tool.name.to_string(), limit),
            None => facts.char_limits.remove(tool.name.as_ref()),
        };
    }
}

/// A positive integer, clamped to the ceiling; anything else is ignored.
fn declared_char_limit(tool: &Tool) -> Option<usize> {
    let value = tool.meta.as_ref()?.get(MAX_RESULT_SIZE_CHARS)?;
    let limit = value.as_u64().filter(|limit| *limit > 0)?;
    Some(usize::try_from(limit).map_or(MAX_CHAR_LIMIT, |l| l.min(MAX_CHAR_LIMIT)))
}

/// Only the JSON Boolean `true` marks; any other value is ignored.
fn marks(tool: &Tool) -> bool {
    tool.meta
        .as_ref()
        .and_then(|meta| meta.get(REQUIRES_USER_INTERACTION))
        .is_some_and(|value| value.as_bool() == Some(true))
}

/// Passes every message through, noting what a `tools/list` response declares and signalling
/// when the server announces its resource or prompt list changed. After the first successful
/// `server/discover` or `initialize` response it adds one `tools/list_changed` notification,
/// which makes rig-rmcp's handler fetch and commit the initial tool list through its own
/// refresh path: the handler's other entry points are not public.
pub(crate) struct MetaTap<T> {
    inner: T,
    meta: ToolMeta,
    queued: Option<ServerJsonRpcMessage>,
    started: bool,
    /// Woken by a `resources/list_changed` or `prompts/list_changed` notification; the
    /// connection's owner re-pulls the listings. A permit is stored when nobody waits yet.
    listings_changed: Arc<Notify>,
}

impl<T> MetaTap<T> {
    pub(crate) fn new(inner: T, meta: ToolMeta, listings_changed: Arc<Notify>) -> Self {
        MetaTap {
            inner,
            meta,
            queued: None,
            started: false,
            listings_changed,
        }
    }
}

impl<T: Transport<RoleClient>> Transport<RoleClient> for MetaTap<T> {
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
        if let Some(message) = self.queued.take() {
            return Some(message);
        }
        let message = self.inner.receive().await?;
        match &message {
            ServerJsonRpcMessage::Response(response)
                if !self.started
                    && matches!(
                        &response.result,
                        ServerResult::DiscoverResult(_) | ServerResult::InitializeResult(_)
                    ) =>
            {
                self.started = true;
                self.queued = Some(ServerJsonRpcMessage::Notification(JsonRpcNotification {
                    jsonrpc: JsonRpcVersion2_0,
                    notification: ServerNotification::ToolListChangedNotification(
                        ToolListChangedNotification::default(),
                    ),
                }));
            }
            ServerJsonRpcMessage::Response(response)
                if matches!(&response.result, ServerResult::ListToolsResult(_)) =>
            {
                if let ServerResult::ListToolsResult(page) = &response.result {
                    for tool in &page.tools {
                        self.meta.record(tool);
                    }
                }
            }
            ServerJsonRpcMessage::Notification(notification) => match &notification.notification {
                ServerNotification::ResourceListChangedNotification(_)
                | ServerNotification::PromptListChangedNotification(_) => {
                    self.listings_changed.notify_one();
                }
                _ => {}
            },
            _ => {}
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
    async fn tap(messages: Vec<ServerJsonRpcMessage>) -> ToolMeta {
        tap_with(messages).await.0
    }

    /// Feeds `messages` through a tap, returning the meta it recorded and the listings-change
    /// signal the notifications woke.
    async fn tap_with(messages: Vec<ServerJsonRpcMessage>) -> (ToolMeta, Arc<Notify>) {
        let marked = ToolMeta::default();
        let changed = Arc::new(Notify::new());
        let mut tap = MetaTap::new(
            Script(messages.iter().cloned().collect()),
            marked.clone(),
            changed.clone(),
        );
        for expected in messages {
            let got = tap.receive().await.expect("a message");
            assert_eq!(
                serde_json::to_value(got).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        }
        assert!(tap.receive().await.is_none());
        (marked, changed)
    }

    #[tokio::test]
    async fn the_first_startup_response_is_followed_by_one_tool_list_change() {
        use rmcp::model::{DiscoverResult, InitializeResult};
        let response = |result| {
            ServerJsonRpcMessage::Response(JsonRpcResponse {
                jsonrpc: JsonRpcVersion2_0,
                id: RequestId::Number(1),
                result,
            })
        };
        let changed = ServerJsonRpcMessage::Notification(rmcp::model::JsonRpcNotification {
            jsonrpc: JsonRpcVersion2_0,
            notification: ServerNotification::ToolListChangedNotification(
                ToolListChangedNotification::default(),
            ),
        });
        for first in [
            ServerResult::DiscoverResult(DiscoverResult::new(vec![], Default::default())),
            ServerResult::InitializeResult(InitializeResult::default()),
        ] {
            let later = response(ServerResult::InitializeResult(InitializeResult::default()));
            let mut tap = MetaTap::new(
                Script(VecDeque::from([response(first), later.clone()])),
                ToolMeta::default(),
                Arc::new(Notify::new()),
            );
            let mut seen = Vec::new();
            while let Some(message) = tap.receive().await {
                seen.push(serde_json::to_value(message).unwrap());
            }
            assert_eq!(seen.len(), 3);
            assert_eq!(seen[1], serde_json::to_value(&changed).unwrap());
            assert_eq!(seen[2], serde_json::to_value(&later).unwrap());
        }
    }

    #[tokio::test]
    async fn a_resource_or_prompt_list_change_wakes_the_listings_signal() {
        use futures::FutureExt;
        use rmcp::model::{PromptListChangedNotification, ResourceListChangedNotification};
        for notification in [
            ServerNotification::ResourceListChangedNotification(
                ResourceListChangedNotification::default(),
            ),
            ServerNotification::PromptListChangedNotification(
                PromptListChangedNotification::default(),
            ),
        ] {
            let message = ServerJsonRpcMessage::Notification(rmcp::model::JsonRpcNotification {
                jsonrpc: JsonRpcVersion2_0,
                notification,
            });
            let (_meta, changed) = tap_with(vec![message]).await;
            assert!(
                changed.notified().now_or_never().is_some(),
                "the notification woke the listings signal"
            );
        }
        // Any other notification leaves it sleeping.
        let other = ServerJsonRpcMessage::Notification(rmcp::model::JsonRpcNotification {
            jsonrpc: JsonRpcVersion2_0,
            notification: ServerNotification::ToolListChangedNotification(
                ToolListChangedNotification::default(),
            ),
        });
        let (_meta, changed) = tap_with(vec![other]).await;
        assert!(changed.notified().now_or_never().is_none());
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
        assert!(marked.requires_user_interaction("yes"));
        for name in ["string", "one", "no", "bare", "absent"] {
            assert!(!marked.requires_user_interaction(name), "{name}");
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
        assert!(marked.requires_user_interaction("a"));
        assert!(marked.requires_user_interaction("b"));
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
        assert!(read(&marked.0, "test").marked.is_empty());
        assert!(read(&marked.0, "test").char_limits.is_empty());
    }

    fn sized(name: &str, value: Option<Value>) -> Tool {
        let mut tool = tool(name, None);
        tool.meta = value.map(|value| {
            let mut meta = MetaObject::new();
            meta.insert(MAX_RESULT_SIZE_CHARS.to_string(), value);
            meta
        });
        tool
    }

    #[tokio::test]
    async fn a_declared_char_limit_is_a_positive_integer_clamped_to_the_ceiling() {
        let meta = tap(vec![page(vec![
            sized("big", Some(json!(200_000))),
            sized("huge", Some(json!(9_000_000))),
            sized("zero", Some(json!(0))),
            sized("negative", Some(json!(-5))),
            sized("fraction", Some(json!(1.5))),
            sized("text", Some(json!("70000"))),
            sized("bare", None),
        ])])
        .await;
        assert_eq!(meta.char_limit("big"), 200_000);
        assert_eq!(meta.char_limit("huge"), MAX_CHAR_LIMIT);
        for name in ["zero", "negative", "fraction", "text", "bare", "absent"] {
            assert_eq!(meta.char_limit(name), DEFAULT_CHAR_LIMIT, "{name}");
        }
    }

    #[tokio::test]
    async fn the_latest_listing_wins_for_char_limits() {
        let meta = tap(vec![
            page(vec![
                sized("a", Some(json!(80_000))),
                sized("b", Some(json!(90_000))),
            ]),
            page(vec![sized("a", Some(json!(70_000)))]),
            page(vec![sized("b", None)]),
        ])
        .await;
        assert_eq!(meta.char_limit("a"), 70_000);
        assert_eq!(meta.char_limit("b"), DEFAULT_CHAR_LIMIT);
    }
}
