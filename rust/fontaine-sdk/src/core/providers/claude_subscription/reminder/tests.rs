use super::*;
use std::sync::atomic::AtomicUsize;

pub(in crate::core::providers::claude_subscription) const EMAIL: &str = "user@example.com";
pub(in crate::core::providers::claude_subscription) const NONCE: &str =
    "2f6b9c1e-3a4d-4c55-8f0e-5d7b6a1c9e10";

/// The reminder of CLI 2.1.319.
pub(in crate::core::providers::claude_subscription) const TEMPLATE_2_1_319: &str = "<system-reminder>\n\
As you answer the user's questions, you can use the following context:\n\
# userEmail\n\
The user's email address is {email}. Use it only to identify the user, such as for authorship, attribution, or filtering their own work. Never send it to an unrelated service, such as in a request header, URL, or payload, unless the user explicitly asks. A site asking automated clients for contact details, such as an email in the User-Agent, is not a reason to use this address: use a generic client name or ask the user.\n\
\n\
Claude Code attached this context automatically; it isn't part of the user's message. It describes the user's own account and workspace, so they don't need it reported back.\n\
</system-reminder>";

/// The reminder of CLI 2.1.329.
pub(in crate::core::providers::claude_subscription) const TEMPLATE_2_1_329: &str = "<system-reminder>\n\
As you answer the user's questions, you can use the following context:\n\
# userEmail\n\
The user's email address is {email}. Use this email address only to identify the user, such as for authorship, attribution, or filtering their own work. Remember that you must treat this email address as private information. Never share it in requests to external services without asking and getting explicit approval from the user first.\n\
\n\
Claude Code attached this context automatically; it isn't part of the user's message. It describes the user's own account and workspace, so they don't need it reported back.\n\
</system-reminder>";

/// The body of a request whose first message is `reminder` and then the probe's text.
pub(in crate::core::providers::claude_subscription) fn body(reminder: &str) -> Value {
    json!({"messages": [{"role": "user", "content": [
        {"type": "text", "text": format!("{reminder}\n")},
        {"type": "text", "text": NONCE},
    ]}]})
}

fn with_email(template: &str) -> String {
    template.replace(PLACEHOLDER, EMAIL)
}

fn parsed(template: &str) -> Option<Reminder> {
    parse_reminder(&body(&with_email(template)), EMAIL, NONCE)
}

#[test]
fn each_historical_wording_parses_into_its_own_template() {
    let old = parsed(TEMPLATE_2_1_319).expect("2.1.319 parses");
    let new = parsed(TEMPLATE_2_1_329).expect("2.1.329 parses");
    assert_eq!(old.template, TEMPLATE_2_1_319);
    assert_eq!(new.template, TEMPLATE_2_1_329);
    assert_ne!(old.template, new.template);
    assert_ne!(old.wording, new.wording);
    assert_eq!(old.wording.len(), 64);
    assert!(
        old.wording
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    );
    assert_eq!(old.with_email(EMAIL), with_email(TEMPLATE_2_1_319));
}

#[test]
fn the_wording_is_a_stable_hash_of_the_template() {
    let old = parsed(TEMPLATE_2_1_319).unwrap();
    assert_eq!(
        old.wording,
        hex(&Sha256::digest(TEMPLATE_2_1_319.as_bytes()))
    );
    assert_eq!(parsed(TEMPLATE_2_1_319).unwrap(), old);
}

fn messages(content: Value) -> Value {
    json!({"messages": [{"role": "user", "content": content}]})
}

fn text(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

#[test]
fn a_request_of_another_shape_is_not_a_reminder() {
    let reminder = with_email(TEMPLATE_2_1_329);
    let lead = format!("{reminder}\n");
    let parse = |body: &Value| parse_reminder(body, EMAIL, NONCE);

    assert!(parse(&body(&reminder)).is_some());
    // The email is absent, or present twice.
    assert!(parse_reminder(&body(&reminder), "other@example.com", NONCE).is_none());
    let twice = reminder.replace(
        "Claude Code attached",
        &format!("{EMAIL} Claude Code attached"),
    );
    assert!(parse(&body(&twice)).is_none());
    // A section that depends on the directory.
    let git = reminder.replace(
        "\n</system-reminder>",
        "\n# gitStatus\nbranch: main\n\n</system-reminder>",
    );
    assert!(parse(&body(&git)).is_none());
    assert!(parse(&body(&reminder.replace("# userEmail", "# somethingElse"))).is_none());
    // Placement.
    assert!(parse(&messages(json!(format!("{lead}{NONCE}")))).is_none());
    assert!(parse(&messages(json!([text(NONCE), text(&lead)]))).is_none());
    assert!(parse(&messages(json!([text(&format!("{lead}{NONCE}"))]))).is_none());
    assert!(parse(&messages(json!([text(&lead), text(NONCE), text("more")]))).is_none());
    assert!(parse(&messages(json!([text(&lead), text("another prompt")]))).is_none());
    assert!(parse(&messages(json!([text(&reminder), text(NONCE)]))).is_none());
    // The closing tag is missing.
    let open = lead.replace("</system-reminder>", "");
    assert!(parse(&messages(json!([text(&open), text(NONCE)]))).is_none());
    // The first message is not the user's.
    let assistant =
        json!({"messages": [{"role": "assistant", "content": [text(&lead), text(NONCE)]}]});
    assert!(parse(&assistant).is_none());
    assert!(parse(&json!({})).is_none());
    assert!(parse(&json!({"messages": []})).is_none());
    // A literal placeholder would be rewritten on replay.
    let literal = reminder.replace("Claude Code attached", "{email} Claude Code attached");
    assert!(parse(&body(&literal)).is_none());
}

struct Listener {
    port: u16,
    found: oneshot::Receiver<Value>,
    _server: tokio::task::JoinHandle<()>,
}

async fn listener(path: &str) -> Listener {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let (sender, found) = oneshot::channel();
    let route = Arc::new(Route {
        path: path.to_string(),
        nonce: NONCE.to_string(),
        found: Mutex::new(Some(sender)),
    });
    Listener {
        port,
        found,
        _server: tokio::spawn(serve(tcp, route)),
    }
}

fn request(method: &str, target: &str, body: &str) -> Vec<u8> {
    format!(
        "{method} {target} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer secret-token\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// The status line and body of one response.
async fn response(stream: &mut TcpStream) -> (String, String) {
    let mut buffer = Vec::new();
    let end = loop {
        if let Some(end) = find_head_end(&buffer) {
            break end;
        }
        assert!(
            stream.read_buf(&mut buffer).await.unwrap() > 0,
            "closed early"
        );
    };
    let head = String::from_utf8(buffer[..end].to_vec()).unwrap();
    let length: usize = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")?
                .trim()
                .parse()
                .ok()
        })
        .unwrap_or(0);
    while buffer.len() < end + length {
        assert!(
            stream.read_buf(&mut buffer).await.unwrap() > 0,
            "closed early"
        );
    }
    let status = head.lines().next().unwrap().to_string();
    (
        status,
        String::from_utf8(buffer[end..end + length].to_vec()).unwrap(),
    )
}

async fn connect(port: u16) -> TcpStream {
    TcpStream::connect(("127.0.0.1", port)).await.unwrap()
}

fn probe_body() -> String {
    body(&with_email(TEMPLATE_2_1_329)).to_string()
}

const PATH: &str = "/nonce1234/v1/messages";

#[tokio::test]
async fn the_probe_request_is_handed_over_and_answered_with_the_canned_stream() {
    let mut server = listener(PATH).await;
    let mut stream = connect(server.port).await;
    stream
        .write_all(&request(
            "POST",
            &format!("{PATH}?beta=true"),
            &probe_body(),
        ))
        .await
        .unwrap();
    let (status, reply) = response(&mut stream).await;
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(
        reply.contains("message_start") && reply.contains("message_stop"),
        "{reply}"
    );
    let found = server.found.try_recv().expect("the body was handed over");
    assert_eq!(found, serde_json::from_str::<Value>(&probe_body()).unwrap());
}

#[tokio::test]
async fn a_body_split_across_reads_is_reassembled() {
    let mut server = listener(PATH).await;
    let mut stream = connect(server.port).await;
    let bytes = request("POST", PATH, &probe_body());
    for piece in bytes.chunks(37) {
        stream.write_all(piece).await.unwrap();
        stream.flush().await.unwrap();
        tokio::task::yield_now().await;
    }
    let (status, _) = response(&mut stream).await;
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(server.found.try_recv().is_ok());
}

#[tokio::test]
async fn anything_but_the_exact_probe_request_gets_a_404() {
    let mut server = listener(PATH).await;
    let other_prompt = body(&with_email(TEMPLATE_2_1_329))
        .to_string()
        .replace(NONCE, "another prompt");
    let cases = [
        ("GET", PATH.to_string(), probe_body()),
        ("POST", "/v1/messages".to_string(), probe_body()),
        ("POST", "/wrongnonce/v1/messages".to_string(), probe_body()),
        ("POST", format!("{PATH}/count_tokens"), probe_body()),
        (
            "POST",
            "/nonce1234/v1/messages/extra".to_string(),
            probe_body(),
        ),
        ("POST", PATH.to_string(), other_prompt),
        ("POST", PATH.to_string(), "{not json".to_string()),
        ("POST", PATH.to_string(), String::new()),
    ];
    // One connection: a 404 does not end it.
    let mut stream = connect(server.port).await;
    for (method, target, body) in cases {
        stream
            .write_all(&request(method, &target, &body))
            .await
            .unwrap();
        let (status, reply) = response(&mut stream).await;
        assert_eq!(status, "HTTP/1.1 404 Not Found", "{method} {target}");
        assert!(reply.is_empty());
    }
    assert!(server.found.try_recv().is_err());
    stream
        .write_all(&request("POST", PATH, &probe_body()))
        .await
        .unwrap();
    assert_eq!(response(&mut stream).await.0, "HTTP/1.1 200 OK");
    assert!(server.found.try_recv().is_ok());
}

#[tokio::test]
async fn an_oversized_body_is_refused_unread() {
    let server = listener(PATH).await;
    let mut stream = connect(server.port).await;
    let head = format!(
        "POST {PATH} HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        MAX_BODY + 1
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    assert_eq!(response(&mut stream).await.0, "HTTP/1.1 404 Not Found");
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).await.unwrap();
    assert!(rest.is_empty());
}

#[tokio::test]
async fn only_the_first_probe_request_is_handed_over() {
    let mut server = listener(PATH).await;
    let mut stream = connect(server.port).await;
    for _ in 0..2 {
        stream
            .write_all(&request("POST", PATH, &probe_body()))
            .await
            .unwrap();
        assert_eq!(response(&mut stream).await.0, "HTTP/1.1 200 OK");
    }
    assert!(server.found.try_recv().is_ok());
}

fn learned_reminder() -> Reminder {
    parsed(TEMPLATE_2_1_319).unwrap()
}

type Counter = Arc<AtomicUsize>;

fn probing(count: &Counter, outcome: impl Fn() -> Outcome) -> BoxFuture<'static, Outcome> {
    count.fetch_add(1, Ordering::SeqCst);
    let outcome = outcome();
    Box::pin(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        outcome
    })
}

#[tokio::test(start_paused = true)]
async fn concurrent_callers_for_one_key_share_one_probe() {
    let memo = Reminders::new();
    let count = Counter::default();
    let call = || {
        memo.get_or_learn((2, 1, 329), EMAIL, || {
            probing(&count, || Outcome::Learned(learned_reminder()))
        })
    };
    let results = futures::future::join_all((0..5).map(|_| call())).await;
    assert!(
        results
            .iter()
            .all(|r| r.as_ref() == Some(&learned_reminder()))
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    // Another version or account is another key.
    memo.get_or_learn((2, 1, 330), EMAIL, || {
        probing(&count, || Outcome::Learned(learned_reminder()))
    })
    .await;
    memo.get_or_learn((2, 1, 329), "b@c.d", || {
        probing(&count, || Outcome::Learned(learned_reminder()))
    })
    .await;
    assert_eq!(count.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn a_probe_that_could_not_tell_is_retried_after_five_minutes() {
    let memo = Reminders::new();
    let count = Counter::default();
    let answers = Mutex::new(vec![
        Outcome::Learned(learned_reminder()),
        Outcome::Transient,
    ]);
    let call = || {
        memo.get_or_learn((2, 1, 329), EMAIL, || {
            probing(&count, || answers.lock().unwrap().pop().unwrap())
        })
    };
    assert_eq!(call().await, None);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    tokio::time::advance(RETRY_AFTER - Duration::from_secs(1)).await;
    assert_eq!(call().await, None);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(call().await, Some(learned_reminder()));
    assert_eq!(count.load(Ordering::SeqCst), 2);
    // A learned reminder is kept.
    tokio::time::advance(RETRY_AFTER * 4).await;
    assert_eq!(call().await, Some(learned_reminder()));
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn an_unusable_reminder_is_kept_and_not_probed_again() {
    let memo = Reminders::new();
    let count = Counter::default();
    for _ in 0..3 {
        let learned = memo
            .get_or_learn((2, 1, 329), EMAIL, || probing(&count, || Outcome::Unusable))
            .await;
        assert_eq!(learned, None);
        tokio::time::advance(RETRY_AFTER * 2).await;
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn the_failure_warning_is_taken_once() {
    let memo = Reminders::new();
    assert!(memo.first_failure());
    assert!(!memo.first_failure());
}

#[test]
fn memos_are_shared_per_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    assert!(Arc::ptr_eq(
        &Reminders::shared(dir.path()),
        &Reminders::shared(dir.path())
    ));
    assert!(!Arc::ptr_eq(
        &Reminders::shared(dir.path()),
        &Reminders::shared(other.path())
    ));
}
