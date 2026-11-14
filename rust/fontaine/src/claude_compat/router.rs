//! The two ends of the SDK stream: one task reads stdin and routes each line, one task owns
//! stdout so messages and control frames never interleave. Requests Fontaine makes of the host
//! (`can_use_tool`, `hook_callback`, `mcp_message`) are correlated by id here, and every one of
//! them fails closed when the host goes away.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fontaine_core::line_reader::{LineReader, ReadFail};
use fontaine_core::steering::{Queued, Steering};
use fontaine_tools::lock::lock;
use futures::future::BoxFuture;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, mpsc, oneshot};

use super::wire::{self, Inbound};
use crate::headless::Cancel;

/// Why a request to the host got no answer. Every one is a denial for the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// The host closed its end, or the run ended.
    Disconnected,
    /// The turn was interrupted, or the host cancelled the request.
    Cancelled,
    /// The host answered with an error.
    Host(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::Disconnected => f.write_str("the host program disconnected"),
            RequestError::Cancelled => f.write_str("the request was cancelled"),
            RequestError::Host(why) => write!(f, "the host answered with an error: {why}"),
        }
    }
}

impl std::error::Error for RequestError {}

/// What a control request from the host is handed to.
pub trait ControlHandler: Send + Sync {
    fn handle(
        &self,
        router: Arc<Router>,
        request_id: String,
        request: Value,
    ) -> BoxFuture<'static, ()>;
}

/// Work for the session loop that needs the idle runtime.
pub enum Command {
    SetModel {
        model: Option<String>,
        reply: oneshot::Sender<Result<String, String>>,
    },
}

/// What the session loop gets next.
pub enum Next {
    Message(String),
    Command(Command),
    Failed(String),
    Eof,
}

#[derive(Default)]
struct Inbox {
    queue: VecDeque<String>,
    commands: VecDeque<Command>,
    turn_active: bool,
    eof: bool,
    /// SIGTERM or SIGINT came: nothing more runs, whatever is still queued or steered.
    shutdown: bool,
    failed: Option<String>,
    /// The first user message or the end of input came, so nothing more is awaited before the
    /// runtime is built.
    started: bool,
    /// An `initialize` was read and has not been answered: the runtime waits for it, because the
    /// user message that follows it on the wire may be read before its handler has run.
    initializing: bool,
}

type Reply = Result<Value, RequestError>;

/// What the writer task is asked to do.
enum Out {
    Line(Value),
    /// Answers once everything before it has been written.
    Flush(oneshot::Sender<()>),
}

pub struct Router {
    out: mpsc::UnboundedSender<Out>,
    pending: Mutex<HashMap<String, oneshot::Sender<Reply>>>,
    next_id: AtomicU64,
    inbox: Mutex<Inbox>,
    wake: Notify,
    idle: Notify,
    pub cancel: Cancel,
    handlers: Mutex<HashMap<String, tokio::task::AbortHandle>>,
    steering: Mutex<Option<Steering>>,
    steer_mid_turn: AtomicBool,
}

/// Cancels a request that is dropped before the host answered, so the host stops waiting for
/// whoever asked.
struct PendingGuard<'a> {
    router: &'a Router,
    id: String,
    done: bool,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let removed = lock(&self.router.pending, "pending requests").remove(&self.id);
        if removed.is_some() {
            self.router.send(wire::control_cancel(&self.id));
        }
    }
}

/// How long the reader waits for a chunk: forever, in effect.
const READ_IDLE: Duration = Duration::from_secs(60 * 60 * 24 * 365);

impl Router {
    /// Starts the reader and the writer over `stdin` and `stdout`.
    pub fn start<R, W>(
        stdin: R,
        stdout: W,
        cancel: Cancel,
        handler: Arc<dyn ControlHandler>,
    ) -> Arc<Router>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out, rx) = mpsc::unbounded_channel();
        let router = Arc::new(Router {
            out,
            pending: Mutex::default(),
            next_id: AtomicU64::new(1),
            inbox: Mutex::default(),
            wake: Notify::new(),
            idle: Notify::new(),
            cancel,
            handlers: Mutex::default(),
            steering: Mutex::new(None),
            steer_mid_turn: AtomicBool::new(true),
        });
        tokio::spawn(write_loop(router.clone(), stdout, rx));
        tokio::spawn(read_loop(router.clone(), stdin, handler));
        router
    }

    /// Writes one line to stdout.
    pub fn send(&self, value: Value) {
        // The writer task owns the pipe; a closed channel means the run is over.
        let _ = self.out.send(Out::Line(value));
    }

    /// Waits until everything sent so far has been written.
    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.out.send(Out::Flush(tx)).is_ok() {
            // A writer that stopped drops the sender, which is as flushed as it will get.
            let _ = rx.await;
        }
    }

    pub fn respond_ok(&self, request_id: &str, response: Value) {
        self.send(wire::control_success(request_id, response));
        lock(&self.handlers, "control handlers").remove(request_id);
    }

    pub fn respond_err(&self, request_id: &str, error: &str) {
        self.send(wire::control_error(request_id, error));
        lock(&self.handlers, "control handlers").remove(request_id);
    }

    /// Asks the host and waits, without a time limit, for its answer.
    pub async fn request(&self, request: Value) -> Reply {
        let id = format!("fontaine-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        {
            let inbox = lock(&self.inbox, "router inbox");
            if inbox.eof || inbox.failed.is_some() {
                return Err(RequestError::Disconnected);
            }
            lock(&self.pending, "pending requests").insert(id.clone(), tx);
        }
        let mut guard = PendingGuard {
            router: self,
            id: id.clone(),
            done: false,
        };
        self.send(wire::control_request(&id, request));
        let answer = rx.await.unwrap_or(Err(RequestError::Disconnected));
        guard.done = true;
        answer
    }

    /// Resolves the request `id`; an answer to nothing is ignored.
    fn resolve(&self, id: &str, answer: Reply) {
        if let Some(tx) = lock(&self.pending, "pending requests").remove(id) {
            // The asker may have given up; its guard already withdrew the request.
            let _ = tx.send(answer);
        }
    }

    /// Fails every outstanding request, so each caller denies.
    fn fail_all(&self, error: &RequestError) {
        let all: Vec<_> = lock(&self.pending, "pending requests").drain().collect();
        for (_, tx) in all {
            let _ = tx.send(Err(error.clone()));
        }
    }

    /// The turn is over for good: the host is gone or the input is unusable.
    pub(super) fn end_input(&self, failure: Option<String>) {
        {
            let mut inbox = lock(&self.inbox, "router inbox");
            inbox.eof = true;
            inbox.started = true;
            inbox.initializing = false;
            if failure.is_some() && inbox.failed.is_none() {
                inbox.failed = failure;
            }
        }
        self.fail_all(&RequestError::Disconnected);
        self.wake.notify_waiters();
    }

    /// The `initialize` request was handled, whatever came of it.
    pub fn initialized(&self) {
        {
            let mut inbox = lock(&self.inbox, "router inbox");
            inbox.started = true;
            inbox.initializing = false;
        }
        self.wake.notify_waiters();
    }

    /// Whether a user message already ran or the runtime was built, so `initialize` is too late.
    pub fn runtime_built(&self) -> bool {
        self.steering_attached()
    }

    fn steering_attached(&self) -> bool {
        lock(&self.steering, "router steering").is_some()
    }

    /// Waits until the `initialize`, the first user message or the end of input has been seen.
    pub async fn wait_started(&self) {
        loop {
            let woken = self.wake.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            {
                let inbox = lock(&self.inbox, "router inbox");
                if inbox.started && !inbox.initializing {
                    return;
                }
            }
            woken.await;
        }
    }

    /// Hands the session's steering queue to the router, which routes a message that arrives
    /// during a turn into it. `mid_turn` false queues every message as the next turn instead.
    pub fn attach(&self, steering: Steering, mid_turn: bool) {
        self.steer_mid_turn.store(mid_turn, Ordering::SeqCst);
        *lock(&self.steering, "router steering") = Some(steering);
    }

    fn push_user(&self, text: String) {
        {
            let mut inbox = lock(&self.inbox, "router inbox");
            inbox.started = true;
            let steering = lock(&self.steering, "router steering").clone();
            match steering {
                Some(steering)
                    if inbox.turn_active && self.steer_mid_turn.load(Ordering::SeqCst) =>
                {
                    steering.queue(Queued::Message(text));
                }
                _ => inbox.queue.push_back(text),
            }
        }
        self.wake.notify_waiters();
    }

    /// A message is waiting: notification turns hold back for it.
    pub fn pending(&self) -> bool {
        let steered = lock(&self.steering, "router steering")
            .as_ref()
            .is_some_and(|s| !s.queued().is_empty());
        let inbox = lock(&self.inbox, "router inbox");
        steered || !inbox.queue.is_empty()
    }

    /// Queues work for the idle runtime.
    pub fn command(&self, command: Command) {
        lock(&self.inbox, "router inbox")
            .commands
            .push_back(command);
        self.wake.notify_waiters();
    }

    /// What the session loop does next. Marks the turn over first, so an interrupt waiting for
    /// the turn to stop is answered, and puts whatever steering never delivered ahead of newer
    /// messages.
    pub async fn next(&self) -> Next {
        loop {
            let woken = self.wake.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            {
                let mut inbox = lock(&self.inbox, "router inbox");
                if std::mem::replace(&mut inbox.turn_active, false) {
                    self.idle.notify_waiters();
                }
                if inbox.shutdown {
                    return Next::Eof;
                }
                let steering = lock(&self.steering, "router steering").clone();
                if let Some(steering) = steering {
                    let undelivered: Vec<String> = steering
                        .retract()
                        .into_iter()
                        .map(|entry| entry.raw().to_string())
                        .collect();
                    for text in undelivered.into_iter().rev() {
                        inbox.queue.push_front(text);
                    }
                }
                if let Some(command) = inbox.commands.pop_front() {
                    return Next::Command(command);
                }
                if let Some(text) = inbox.queue.pop_front() {
                    inbox.turn_active = true;
                    self.cancel.reset();
                    return Next::Message(text);
                }
                if let Some(why) = inbox.failed.clone() {
                    return Next::Failed(why);
                }
                if inbox.eof {
                    return Next::Eof;
                }
            }
            woken.await;
        }
    }

    /// Stops the running turn and waits until it has stopped. With no turn running there is
    /// nothing to stop.
    pub async fn interrupt(&self) {
        if !lock(&self.inbox, "router inbox").turn_active {
            return;
        }
        self.cancel.trigger();
        loop {
            let stopped = self.idle.notified();
            tokio::pin!(stopped);
            stopped.as_mut().enable();
            if !lock(&self.inbox, "router inbox").turn_active {
                return;
            }
            stopped.await;
        }
    }

    /// SIGTERM or SIGINT: the turn stops as an interrupt stops it, nothing queued runs, and the
    /// input ends, so the run tears down its jobs and subagents and exits.
    pub fn shut_down(&self) {
        {
            let mut inbox = lock(&self.inbox, "router inbox");
            inbox.queue.clear();
            inbox.shutdown = true;
        }
        self.cancel.trigger();
        self.end_input(None);
    }

    #[cfg(test)]
    fn input_ended(&self) -> bool {
        lock(&self.inbox, "router inbox").eof
    }

    /// Ends the run: stdout is flushed by the writer once every sender is gone.
    pub fn close(&self) {
        self.fail_all(&RequestError::Disconnected);
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    router: Arc<Router>,
    mut stdout: W,
    mut rx: mpsc::UnboundedReceiver<Out>,
) {
    while let Some(next) = rx.recv().await {
        let value = match next {
            Out::Line(value) => value,
            Out::Flush(done) => {
                let _ = done.send(());
                continue;
            }
        };
        let mut bytes = serde_json::to_vec(&value).expect("a JSON value serializes");
        bytes.push(b'\n');
        let wrote = async {
            stdout.write_all(&bytes).await?;
            stdout.flush().await
        };
        if wrote.await.is_err() {
            // A closed stdout is the host's decision: nothing more can be said, so the run
            // stops as an interrupt does and its input ends.
            router.cancel.trigger();
            router.end_input(None);
            return;
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    router: Arc<Router>,
    stdin: R,
    handler: Arc<dyn ControlHandler>,
) {
    let mut lines = LineReader::new(stdin);
    loop {
        let line = match lines.next_line(READ_IDLE).await {
            Ok(Some(line)) => line,
            Ok(None) | Err(ReadFail::Idle) => {
                router.end_input(None);
                return;
            }
            Err(ReadFail::TooLong) => {
                router.end_input(Some("an input line is too long".to_string()));
                return;
            }
            Err(ReadFail::Io) => {
                router.end_input(None);
                return;
            }
        };
        match wire::parse_line(&line) {
            Ok(Inbound::User(message)) => router.push_user(message.text),
            Ok(Inbound::ControlRequest {
                request_id,
                request,
            }) => {
                if request.get("subtype").and_then(Value::as_str) == Some("initialize") {
                    lock(&router.inbox, "router inbox").initializing = true;
                }
                // Held across the spawn: a handler that answers at once waits for its entry.
                let mut handlers = lock(&router.handlers, "control handlers");
                let task =
                    tokio::spawn(handler.handle(router.clone(), request_id.clone(), request));
                handlers.insert(request_id, task.abort_handle());
            }
            Ok(Inbound::ControlResponse(response)) => {
                router.resolve(
                    &response.request_id,
                    response.outcome.map_err(RequestError::Host),
                );
            }
            Ok(Inbound::ControlCancel { request_id }) => {
                // The host abandons a request of ours, or one of its own.
                router.resolve(&request_id, Err(RequestError::Cancelled));
                if let Some(handle) = lock(&router.handlers, "control handlers").remove(&request_id)
                {
                    handle.abort();
                }
            }
            Ok(Inbound::KeepAlive) => {}
            Err(malformed) => {
                router.end_input(Some(malformed.0));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, BufReader};

    struct Echo;

    impl ControlHandler for Echo {
        fn handle(
            &self,
            router: Arc<Router>,
            request_id: String,
            request: Value,
        ) -> BoxFuture<'static, ()> {
            Box::pin(async move {
                if request["subtype"] == "interrupt" {
                    router.interrupt().await;
                }
                router.respond_ok(&request_id, request);
            })
        }
    }

    struct Rig {
        router: Arc<Router>,
        host_in: tokio::io::DuplexStream,
        host_out: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    }

    fn rig() -> Rig {
        let (host_in, stdin) = tokio::io::duplex(1 << 16);
        let (stdout, host_out) = tokio::io::duplex(1 << 16);
        let router = Router::start(stdin, stdout, Cancel::default(), Arc::new(Echo));
        Rig {
            router,
            host_in,
            host_out: BufReader::new(host_out).lines(),
        }
    }

    async fn say(host: &mut tokio::io::DuplexStream, value: Value) {
        host.write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
    }

    async fn line(rig: &mut Rig) -> Value {
        let text = rig.host_out.next_line().await.unwrap().unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn user(text: &str) -> Value {
        json!({"type":"user","message":{"role":"user","content":text}})
    }

    #[tokio::test]
    async fn a_request_is_answered_by_its_id() {
        let mut rig = rig();
        let router = rig.router.clone();
        let asked =
            tokio::spawn(async move { router.request(json!({"subtype": "can_use_tool"})).await });
        let sent = line(&mut rig).await;
        let id = sent["request_id"].as_str().unwrap().to_string();
        say(&mut rig.host_in, json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":{"behavior":"allow"}}})).await;
        assert_eq!(asked.await.unwrap(), Ok(json!({"behavior": "allow"})));
    }

    #[tokio::test]
    async fn a_host_error_and_a_disconnect_fail_the_request() {
        let mut rig = rig();
        let router = rig.router.clone();
        let asked = tokio::spawn(async move { router.request(json!({"subtype": "x"})).await });
        let id = line(&mut rig).await["request_id"]
            .as_str()
            .unwrap()
            .to_string();
        say(&mut rig.host_in, json!({"type":"control_response","response":{"subtype":"error","request_id":id,"error":"nope"}})).await;
        assert_eq!(asked.await.unwrap(), Err(RequestError::Host("nope".into())));

        let router = rig.router.clone();
        let waiting = tokio::spawn(async move { router.request(json!({"subtype": "y"})).await });
        let _ = line(&mut rig).await;
        drop(rig.host_in);
        assert_eq!(waiting.await.unwrap(), Err(RequestError::Disconnected));
        assert_eq!(
            rig.router.request(json!({"subtype": "z"})).await,
            Err(RequestError::Disconnected)
        );
    }

    #[tokio::test]
    async fn a_dropped_request_tells_the_host_to_stop_waiting() {
        let mut rig = rig();
        let router = rig.router.clone();
        let asked = tokio::spawn(async move { router.request(json!({"subtype": "x"})).await });
        let id = line(&mut rig).await["request_id"]
            .as_str()
            .unwrap()
            .to_string();
        asked.abort();
        let cancel = line(&mut rig).await;
        assert_eq!(cancel["type"], "control_cancel_request");
        assert_eq!(cancel["request_id"], id);
    }

    #[tokio::test]
    async fn a_control_request_is_served_while_the_session_loop_is_busy() {
        let mut rig = rig();
        say(&mut rig.host_in, user("go")).await;
        assert!(matches!(rig.router.next().await, Next::Message(text) if text == "go"));
        // A turn is running: nobody has asked for the next message yet.
        say(
            &mut rig.host_in,
            json!({"type":"control_request","request_id":"c1","request":{"subtype":"mcp_status"}}),
        )
        .await;
        let response = line(&mut rig).await;
        assert_eq!(response["response"]["request_id"], "c1");
    }

    #[tokio::test]
    async fn an_interrupt_is_answered_once_the_turn_has_stopped() {
        let mut rig = rig();
        say(&mut rig.host_in, user("go")).await;
        assert!(matches!(rig.router.next().await, Next::Message(_)));
        say(
            &mut rig.host_in,
            json!({"type":"control_request","request_id":"i1","request":{"subtype":"interrupt"}}),
        )
        .await;
        // The turn stops when the loop comes back for the next message.
        tokio::time::timeout(Duration::from_secs(5), async {
            while !rig.router.cancel.is_set() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let router = rig.router.clone();
        let next = tokio::spawn(async move { router.next().await });
        let response = line(&mut rig).await;
        assert_eq!(response["response"]["request_id"], "i1");
        drop(rig.host_in);
        assert!(matches!(next.await.unwrap(), Next::Eof));
    }

    #[tokio::test]
    async fn input_keeps_arrival_order_and_a_malformed_line_ends_it() {
        let mut rig = rig();
        say(&mut rig.host_in, user("one")).await;
        say(&mut rig.host_in, user("two")).await;
        assert!(matches!(rig.router.next().await, Next::Message(t) if t == "one"));
        assert!(matches!(rig.router.next().await, Next::Message(t) if t == "two"));
        rig.host_in.write_all(b"not json\n").await.unwrap();
        assert!(matches!(rig.router.next().await, Next::Failed(_)));
    }

    #[tokio::test]
    async fn a_message_during_a_turn_is_steered_and_a_leftover_becomes_the_next_turn() {
        let mut rig = rig();
        let steering = Steering::default();
        rig.router.attach(steering.clone(), true);
        say(&mut rig.host_in, user("first")).await;
        assert!(matches!(rig.router.next().await, Next::Message(t) if t == "first"));
        say(&mut rig.host_in, user("late")).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while steering.queued().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(rig.router.pending());
        assert!(matches!(rig.router.next().await, Next::Message(t) if t == "late"));
    }

    #[tokio::test]
    async fn with_steering_off_a_message_during_a_turn_waits_as_the_next_turn() {
        let mut rig = rig();
        let steering = Steering::default();
        rig.router.attach(steering.clone(), false);
        say(&mut rig.host_in, user("first")).await;
        assert!(matches!(rig.router.next().await, Next::Message(_)));
        say(&mut rig.host_in, user("second")).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !rig.router.pending() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(steering.queued().is_empty());
        assert!(matches!(rig.router.next().await, Next::Message(t) if t == "second"));
    }

    #[tokio::test]
    async fn after_shutdown_a_steered_message_never_starts_a_turn() {
        let mut rig = rig();
        let steering = Steering::default();
        rig.router.attach(steering.clone(), true);
        say(&mut rig.host_in, user("first")).await;
        assert!(matches!(rig.router.next().await, Next::Message(_)));
        say(&mut rig.host_in, user("late")).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while steering.queued().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        rig.router.shut_down();
        assert!(matches!(rig.router.next().await, Next::Eof));
    }

    #[tokio::test]
    async fn keep_alive_lines_are_ignored_and_the_end_of_input_is_eof() {
        let mut rig = rig();
        say(&mut rig.host_in, json!({"type":"keep_alive"})).await;
        say(&mut rig.host_in, user("a")).await;
        assert!(matches!(rig.router.next().await, Next::Message(t) if t == "a"));
        drop(rig.host_in);
        assert!(matches!(rig.router.next().await, Next::Eof));
        assert!(rig.router.input_ended());
    }
}
