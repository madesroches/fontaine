//! MCP servers that live in the host program. Fontaine speaks JSON-RPC to them over an in-process
//! pipe; each message is tunnelled to the host as an `mcp_message` control request and the
//! host's `mcp_response` is written back. The tools register like any MCP tool: `Arbitrary`,
//! through the full gate, with untrusted results.

use std::sync::Arc;

use fontaine_core::line_reader::LineReader;
use fontaine_tools::mcp::SdkTransportFactory;
use serde_json::{Value, json};
use tokio::io::{AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::sync::Mutex;

use super::router::Router;

pub struct SdkServers {
    pub router: Arc<Router>,
}

/// How much one pipe holds before a writer waits.
const PIPE_BYTES: usize = 1 << 20;

impl SdkTransportFactory for SdkServers {
    fn open(&self, server: &str) -> Result<DuplexStream, String> {
        let (client, host_end) = tokio::io::duplex(PIPE_BYTES);
        let (read, write) = tokio::io::split(host_end);
        let router = self.router.clone();
        let server = server.to_string();
        tokio::spawn(async move {
            let write = Arc::new(Mutex::new(write));
            let mut lines = LineReader::new(read);
            // A closed pipe is the connection ending: the loop stops with it.
            while let Ok(Some(line)) = lines
                .next_line(std::time::Duration::from_secs(60 * 60 * 24 * 365))
                .await
            {
                let Ok(message) = serde_json::from_slice::<Value>(&line) else {
                    continue;
                };
                let (router, server, write) = (router.clone(), server.clone(), write.clone());
                tokio::spawn(async move { forward(&router, &server, message, &write).await });
            }
        });
        Ok(client)
    }
}

/// Sends one JSON-RPC message to the host's server and writes its answer back, when it has one.
async fn forward<W: AsyncWrite + Unpin>(
    router: &Router,
    server: &str,
    message: Value,
    write: &Mutex<W>,
) {
    let id = message.get("id").cloned();
    let reply = router
        .request(json!({
            "subtype": "mcp_message",
            "server_name": server,
            "message": message,
        }))
        .await;
    // A notification has no answer to carry back.
    let Some(id) = id else { return };
    let response = match reply {
        Ok(reply) => reply
            .get("mcp_response")
            .cloned()
            .unwrap_or_else(|| json!({"jsonrpc": "2.0", "id": id, "result": {}})),
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32000, "message": e.to_string() },
        }),
    };
    let mut bytes = serde_json::to_vec(&response).expect("a JSON value serializes");
    bytes.push(b'\n');
    let mut write = write.lock().await;
    // The pipe closing means the connection is gone and nobody waits for the answer.
    let _ = write.write_all(&bytes).await;
    let _ = write.flush().await;
}

#[cfg(test)]
mod tests {
    use super::super::router::ControlHandler;
    use super::*;
    use crate::headless::Cancel;
    use futures::future::BoxFuture;
    use tokio::io::{AsyncBufReadExt, BufReader};

    struct Quiet;
    impl ControlHandler for Quiet {
        fn handle(&self, _: Arc<Router>, _: String, _: Value) -> BoxFuture<'static, ()> {
            Box::pin(async {})
        }
    }

    #[tokio::test]
    async fn a_request_goes_to_the_host_and_its_response_comes_back_on_the_pipe() {
        let (mut host_in, stdin) = tokio::io::duplex(1 << 16);
        let (stdout, host_out) = tokio::io::duplex(1 << 16);
        let router = Router::start(stdin, stdout, Cancel::default(), Arc::new(Quiet));
        let servers = SdkServers { router };
        let mut client = servers.open("calc").unwrap();
        client
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/list\"}\n")
            .await
            .unwrap();
        let mut host_lines = BufReader::new(host_out).lines();
        let sent: Value =
            serde_json::from_str(&host_lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(sent["request"]["subtype"], "mcp_message");
        assert_eq!(sent["request"]["server_name"], "calc");
        assert_eq!(sent["request"]["message"]["method"], "tools/list");
        let id = sent["request_id"].as_str().unwrap();
        let reply = json!({"type":"control_response","response":{"subtype":"success","request_id":id,
            "response":{"mcp_response":{"jsonrpc":"2.0","id":7,"result":{"tools":[]}}}}});
        host_in
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();
        let mut back = BufReader::new(client).lines();
        let answer: Value =
            serde_json::from_str(&back.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(answer["id"], 7);
        assert_eq!(answer["result"]["tools"], json!([]));
    }

    #[tokio::test]
    async fn a_disconnected_host_answers_with_a_json_rpc_error() {
        let (host_in, stdin) = tokio::io::duplex(1 << 16);
        let (stdout, _host_out) = tokio::io::duplex(1 << 16);
        let router = Router::start(stdin, stdout, Cancel::default(), Arc::new(Quiet));
        let servers = SdkServers { router };
        let mut client = servers.open("calc").unwrap();
        drop(host_in);
        client
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
            .await
            .unwrap();
        let mut back = BufReader::new(client).lines();
        let answer: Value =
            serde_json::from_str(&back.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(answer["id"], 1);
        assert!(answer["error"].is_object());
    }
}
