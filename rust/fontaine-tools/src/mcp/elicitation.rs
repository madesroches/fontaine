//! Serving an MCP server's `elicitation/create` request: a form shown to the human. rig-rmcp's
//! client handler declines every elicitation and cannot be wrapped, so the request is taken off
//! the wire by a transport wrapper before rmcp sees it, and the answer is written back as the
//! response. Nothing a person types here reaches the model, the transcript or the audit log.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use futures::future::BoxFuture;
use rmcp::RoleClient;
use rmcp::model::{
    ClientJsonRpcMessage, ClientResult, ElicitRequestParams, ElicitResult, ElicitationAction,
    EnumSchema, MultiSelectEnumSchema, PrimitiveSchemaDefinition, RequestId, ServerJsonRpcMessage,
    ServerNotification, ServerRequest, SingleSelectEnumSchema, StringFormat,
};
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use serde_json::{Map, Value};
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};

pub use rmcp::model::ElicitationSchema;

const TARGET: &str = "fontaine::telemetry";

/// A server's request for input, with the server's name for the dialog.
#[derive(Clone, Debug)]
pub struct ElicitForm {
    pub server: String,
    /// Untrusted text from the server.
    pub message: String,
    pub schema: ElicitationSchema,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ElicitReply {
    /// Values for the form's fields, already valid for its schema.
    Accept(Map<String, Value>),
    Decline,
    Cancel,
}

/// Whoever answers an elicitation: the terminal UI, or nobody.
pub trait Elicitor: Send + Sync {
    fn elicit<'a>(&'a self, form: ElicitForm) -> BoxFuture<'a, ElicitReply>;
}

/// Answers every elicitation with a cancel: the doctor probe, headless runs and tests.
pub struct CancelAll;

impl Elicitor for CancelAll {
    fn elicit<'a>(&'a self, _form: ElicitForm) -> BoxFuture<'a, ElicitReply> {
        Box::pin(async { ElicitReply::Cancel })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextFormat {
    Email,
    Uri,
    Date,
    DateTime,
}

impl TextFormat {
    pub fn name(self) -> &'static str {
        match self {
            TextFormat::Email => "email",
            TextFormat::Uri => "uri",
            TextFormat::Date => "date",
            TextFormat::DateTime => "date-time",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    Text {
        min_length: Option<u32>,
        max_length: Option<u32>,
        format: Option<TextFormat>,
        default: Option<String>,
    },
    Number {
        minimum: Option<f64>,
        maximum: Option<f64>,
        default: Option<f64>,
    },
    Integer {
        minimum: Option<i64>,
        maximum: Option<i64>,
        default: Option<i64>,
    },
    Boolean {
        default: bool,
    },
    Select {
        options: Vec<Choice>,
        default: Option<String>,
    },
    MultiSelect {
        options: Vec<Choice>,
        min_items: Option<u64>,
        max_items: Option<u64>,
        default: Vec<String>,
    },
}

/// One row of the form.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub key: String,
    /// The property's title, or its key.
    pub title: String,
    pub description: Option<String>,
    pub required: bool,
    pub kind: FieldKind,
}

fn choices(values: &[String], names: Option<&Vec<String>>) -> Vec<Choice> {
    values
        .iter()
        .enumerate()
        .map(|(i, value)| Choice {
            value: value.clone(),
            title: names
                .and_then(|names| names.get(i))
                .cloned()
                .unwrap_or_else(|| value.clone()),
        })
        .collect()
}

fn titled(options: &[rmcp::model::ConstTitle]) -> Vec<Choice> {
    options
        .iter()
        .map(|option| Choice {
            value: option.const_.clone(),
            title: option.title.clone(),
        })
        .collect()
}

fn text_format(format: StringFormat) -> Option<TextFormat> {
    match format {
        StringFormat::Email => Some(TextFormat::Email),
        StringFormat::Uri => Some(TextFormat::Uri),
        StringFormat::Date => Some(TextFormat::Date),
        StringFormat::DateTime => Some(TextFormat::DateTime),
        // A format this client does not know cannot be checked, so it is not enforced.
        _ => None,
    }
}

/// A field's kind, title and description from its schema.
fn describe(definition: &PrimitiveSchemaDefinition) -> (FieldKind, Option<String>, Option<String>) {
    let text =
        |value: &Option<std::borrow::Cow<'static, str>>| value.as_ref().map(|v| v.to_string());
    match definition {
        PrimitiveSchemaDefinition::String(s) => (
            FieldKind::Text {
                min_length: s.min_length,
                max_length: s.max_length,
                format: s.format.and_then(text_format),
                default: s.default.clone(),
            },
            text(&s.title),
            text(&s.description),
        ),
        PrimitiveSchemaDefinition::Number(n) => (
            FieldKind::Number {
                minimum: n.minimum,
                maximum: n.maximum,
                default: n.default,
            },
            text(&n.title),
            text(&n.description),
        ),
        PrimitiveSchemaDefinition::Integer(n) => (
            FieldKind::Integer {
                minimum: n.minimum,
                maximum: n.maximum,
                default: n.default,
            },
            text(&n.title),
            text(&n.description),
        ),
        PrimitiveSchemaDefinition::Boolean(b) => (
            FieldKind::Boolean {
                default: b.default.unwrap_or(false),
            },
            text(&b.title),
            text(&b.description),
        ),
        PrimitiveSchemaDefinition::Enum(EnumSchema::Single(single)) => match single {
            SingleSelectEnumSchema::Untitled(e) => (
                FieldKind::Select {
                    options: choices(&e.enum_, None),
                    default: e.default.clone(),
                },
                text(&e.title),
                text(&e.description),
            ),
            SingleSelectEnumSchema::Titled(e) => (
                FieldKind::Select {
                    options: titled(&e.one_of),
                    default: e.default.clone(),
                },
                text(&e.title),
                text(&e.description),
            ),
            _ => unknown_kind(),
        },
        PrimitiveSchemaDefinition::Enum(EnumSchema::Multi(multi)) => match multi {
            MultiSelectEnumSchema::Untitled(e) => (
                FieldKind::MultiSelect {
                    options: choices(&e.items.enum_, None),
                    min_items: e.min_items,
                    max_items: e.max_items,
                    default: e.default.clone().unwrap_or_default(),
                },
                text(&e.title),
                text(&e.description),
            ),
            MultiSelectEnumSchema::Titled(e) => (
                FieldKind::MultiSelect {
                    options: titled(&e.items.any_of),
                    min_items: e.min_items,
                    max_items: e.max_items,
                    default: e.default.clone().unwrap_or_default(),
                },
                text(&e.title),
                text(&e.description),
            ),
            _ => unknown_kind(),
        },
        PrimitiveSchemaDefinition::Enum(EnumSchema::Legacy(e)) => (
            FieldKind::Select {
                options: choices(&e.enum_, e.enum_names.as_ref()),
                default: e.default.clone(),
            },
            text(&e.title),
            text(&e.description),
        ),
        _ => unknown_kind(),
    }
}

/// A schema shape this client does not know is offered as free text.
fn unknown_kind() -> (FieldKind, Option<String>, Option<String>) {
    (
        FieldKind::Text {
            min_length: None,
            max_length: None,
            format: None,
            default: None,
        },
        None,
        None,
    )
}

impl FieldKind {
    /// The JSON value a person's `input` stands for in a text, number or integer field; `None`
    /// for an empty one.
    pub fn value_of_text(&self, input: &str) -> Result<Option<Value>, String> {
        let trimmed = input.trim();
        match self {
            FieldKind::Text { .. } => Ok((!input.is_empty()).then(|| Value::String(input.into()))),
            FieldKind::Number { .. } if trimmed.is_empty() => Ok(None),
            FieldKind::Number { .. } => trimmed
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .and_then(serde_json::Number::from_f64)
                .map(|n| Some(Value::Number(n)))
                .ok_or_else(|| "enter a number".to_string()),
            FieldKind::Integer { .. } if trimmed.is_empty() => Ok(None),
            FieldKind::Integer { .. } => trimmed
                .parse::<i64>()
                .map(|n| Some(Value::from(n)))
                .map_err(|_| "enter a whole number".to_string()),
            _ => Err("this field is not typed".to_string()),
        }
    }
}

impl ElicitForm {
    /// The rows of the form, in the schema's property order.
    pub fn fields(&self) -> Vec<Field> {
        let schema = &self.schema;
        let required: BTreeSet<&str> = schema
            .required
            .iter()
            .flatten()
            .map(String::as_str)
            .collect();
        let mut keys: Vec<&String> = Vec::new();
        for key in schema.property_order.iter().flatten() {
            if schema.properties.contains_key(key) && !keys.contains(&key) {
                keys.push(key);
            }
        }
        for key in schema.properties.keys() {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys.into_iter()
            .map(|key| {
                let (kind, title, description) = describe(&schema.properties[key]);
                Field {
                    key: key.clone(),
                    title: title.unwrap_or_else(|| key.clone()),
                    description,
                    required: required.contains(key.as_str()),
                    kind,
                }
            })
            .collect()
    }

    /// Whether `answer` is a valid answer to this form: what the dialog checks as the person
    /// types and the tap checks again before anything is sent.
    pub fn validate(&self, answer: &Map<String, Value>) -> Result<(), String> {
        let fields = self.fields();
        for key in answer.keys() {
            if !fields.iter().any(|field| &field.key == key) {
                return Err(format!("`{key}` is not a field of this form"));
            }
        }
        for field in &fields {
            match answer.get(&field.key) {
                None | Some(Value::Null) => {
                    if field.required {
                        return Err(format!("{} is required", field.title));
                    }
                }
                Some(value) => check(field, value)?,
            }
        }
        Ok(())
    }
}

fn check(field: &Field, value: &Value) -> Result<(), String> {
    let title = &field.title;
    match &field.kind {
        FieldKind::Text {
            min_length,
            max_length,
            format,
            ..
        } => {
            let Some(text) = value.as_str() else {
                return Err(format!("{title} must be text"));
            };
            let length = text.chars().count();
            if min_length.is_some_and(|min| length < min as usize) {
                return Err(format!(
                    "{title} must be at least {} characters",
                    min_length.unwrap_or_default()
                ));
            }
            if max_length.is_some_and(|max| length > max as usize) {
                return Err(format!(
                    "{title} must be at most {} characters",
                    max_length.unwrap_or_default()
                ));
            }
            if let Some(format) = format
                && !format_ok(*format, text)
            {
                return Err(format!("{title} must be a valid {}", format.name()));
            }
            Ok(())
        }
        FieldKind::Number {
            minimum, maximum, ..
        } => {
            let Some(n) = value.as_f64() else {
                return Err(format!("{title} must be a number"));
            };
            if minimum.is_some_and(|min| n < min) {
                return Err(format!(
                    "{title} must be at least {}",
                    minimum.unwrap_or(0.0)
                ));
            }
            if maximum.is_some_and(|max| n > max) {
                return Err(format!(
                    "{title} must be at most {}",
                    maximum.unwrap_or(0.0)
                ));
            }
            Ok(())
        }
        FieldKind::Integer {
            minimum, maximum, ..
        } => {
            let Some(n) = value.as_i64() else {
                return Err(format!("{title} must be a whole number"));
            };
            if minimum.is_some_and(|min| n < min) {
                return Err(format!("{title} must be at least {}", minimum.unwrap_or(0)));
            }
            if maximum.is_some_and(|max| n > max) {
                return Err(format!("{title} must be at most {}", maximum.unwrap_or(0)));
            }
            Ok(())
        }
        FieldKind::Boolean { .. } => value
            .as_bool()
            .map(|_| ())
            .ok_or_else(|| format!("{title} must be true or false")),
        FieldKind::Select { options, .. } => {
            let allowed = value
                .as_str()
                .is_some_and(|v| options.iter().any(|option| option.value == v));
            if allowed {
                Ok(())
            } else {
                Err(format!("{title} must be one of the offered choices"))
            }
        }
        FieldKind::MultiSelect {
            options,
            min_items,
            max_items,
            ..
        } => {
            let Some(items) = value.as_array() else {
                return Err(format!("{title} must be a list of choices"));
            };
            let known = items.iter().all(|item| {
                item.as_str()
                    .is_some_and(|v| options.iter().any(|option| option.value == v))
            });
            if !known {
                return Err(format!("{title} must be offered choices"));
            }
            if min_items.is_some_and(|min| (items.len() as u64) < min) {
                return Err(format!(
                    "{title} needs at least {} choices",
                    min_items.unwrap_or_default()
                ));
            }
            if max_items.is_some_and(|max| items.len() as u64 > max) {
                return Err(format!(
                    "{title} allows at most {} choices",
                    max_items.unwrap_or_default()
                ));
            }
            Ok(())
        }
    }
}

fn format_ok(format: TextFormat, text: &str) -> bool {
    match format {
        TextFormat::Email => email_ok(text),
        TextFormat::Uri => url::Url::parse(text).is_ok(),
        TextFormat::Date => date_ok(text),
        TextFormat::DateTime => date_time_ok(text),
    }
}

fn email_ok(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !text.chars().any(char::is_whitespace)
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
}

fn digits(text: &str, count: usize) -> Option<u32> {
    (text.len() == count && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

fn date_ok(text: &str) -> bool {
    let mut parts = text.split('-');
    let (Some(y), Some(m), Some(d), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Some(year), Some(month), Some(day)) = (digits(y, 4), digits(m, 2), digits(d, 2)) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

/// RFC 3339: `YYYY-MM-DDTHH:MM:SS[.fraction]` then `Z` or `+HH:MM` / `-HH:MM`.
fn date_time_ok(text: &str) -> bool {
    let Some((date, rest)) = text.split_once(['T', 't']) else {
        return false;
    };
    if !date_ok(date) {
        return false;
    }
    let (time, offset) = if let Some(time) = rest.strip_suffix(['Z', 'z']) {
        (time, None)
    } else if let Some(at) = rest.rfind(['+', '-']) {
        (&rest[..at], Some(&rest[at + 1..]))
    } else {
        return false;
    };
    if let Some(offset) = offset {
        let Some((h, m)) = offset.split_once(':') else {
            return false;
        };
        if !matches!((digits(h, 2), digits(m, 2)), (Some(h), Some(m)) if h < 24 && m < 60) {
            return false;
        }
    }
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    if fraction.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit())) {
        return false;
    }
    let mut parts = clock.split(':');
    let (Some(h), Some(m), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    matches!(
        (digits(h, 2), digits(m, 2), digits(s, 2)),
        (Some(h), Some(m), Some(s)) if h < 24 && m < 60 && s <= 60
    )
}

/// Calls in flight on one connection, by who made them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InFlight {
    pub main: usize,
    pub sub: usize,
}

impl InFlight {
    fn total(self) -> usize {
        self.main + self.sub
    }
}

/// Shared by a connection's guarded tools, which count the calls they run, and its elicitation
/// tap, which has to know whether one is running and whose it is: an elicitation names no caller.
#[derive(Clone)]
pub(crate) struct CallTracker(Arc<watch::Sender<InFlight>>);

impl CallTracker {
    pub(crate) fn new() -> Self {
        CallTracker(Arc::new(watch::Sender::new(InFlight::default())))
    }

    pub(crate) fn enter(&self, root: bool) -> CallGuard {
        self.0.send_modify(|flight| {
            if root {
                flight.main += 1;
            } else {
                flight.sub += 1;
            }
        });
        CallGuard {
            tracker: self.clone(),
            root,
        }
    }

    fn subscribe(&self) -> watch::Receiver<InFlight> {
        self.0.subscribe()
    }

    /// Waits until no call is in flight. The caller bounds the wait.
    pub(crate) async fn idle(&self) {
        let mut flight = self.subscribe();
        // The sender lives as long as this tracker, so the wait only ends on idle.
        let _ = flight.wait_for(|flight| flight.total() == 0).await;
    }
}

/// Counts one call in flight until dropped. What a non-tool caller holds when it joins the
/// tracker's counting, as a resource read or a prompt fetch does.
pub struct CallGuard {
    tracker: CallTracker,
    root: bool,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        let root = self.root;
        self.tracker.0.send_modify(|flight| {
            let count = if root {
                &mut flight.main
            } else {
                &mut flight.sub
            };
            *count = count.saturating_sub(1);
        });
    }
}

struct Pending {
    abort: AbortHandle,
    form: ElicitForm,
    /// A call was in flight when it arrived, so it ends with the last call.
    during_call: bool,
}

struct Answer {
    id: RequestId,
    reply: ElicitReply,
}

/// Serves form elicitations off the wire and passes everything else through unchanged.
pub(crate) struct ElicitationTap<T> {
    inner: T,
    server: String,
    /// The server name as telemetry may show it.
    shown: String,
    elicitor: Arc<dyn Elicitor>,
    calls: watch::Receiver<InFlight>,
    calls_open: bool,
    pending: HashMap<RequestId, Pending>,
    // Dropping the set aborts every task still waiting on a person.
    tasks: JoinSet<()>,
    answers: mpsc::UnboundedReceiver<Answer>,
    answer_sender: mpsc::UnboundedSender<Answer>,
}

impl<T> ElicitationTap<T> {
    pub(crate) fn new(
        inner: T,
        server: &str,
        shown: &str,
        elicitor: Arc<dyn Elicitor>,
        tracker: &CallTracker,
    ) -> Self {
        let (answer_sender, answers) = mpsc::unbounded_channel();
        ElicitationTap {
            inner,
            server: server.to_string(),
            shown: shown.to_string(),
            elicitor,
            calls: tracker.subscribe(),
            calls_open: true,
            pending: HashMap::new(),
            tasks: JoinSet::new(),
            answers,
            answer_sender,
        }
    }
}

fn action_name(action: &ElicitationAction) -> &'static str {
    match action {
        ElicitationAction::Accept => "accept",
        ElicitationAction::Decline => "decline",
        ElicitationAction::Cancel => "cancel",
        _ => "cancel",
    }
}

impl<T: Transport<RoleClient>> ElicitationTap<T> {
    /// Writes `result` as the response to request `id`. rmcp drops the `receive` future whenever
    /// another `select!` branch wins, so an awaited send could be dropped half done; the send's
    /// own future is `'static` and runs on its own task, as rmcp's does.
    fn respond(&mut self, id: RequestId, result: ElicitResult) {
        tracing::info!(
            target: TARGET,
            server = self.shown.as_str(),
            action = action_name(&result.action),
            "mcp_elicitation"
        );
        let message: ClientJsonRpcMessage =
            ClientJsonRpcMessage::response(ClientResult::ElicitResult(result), id);
        let sending = self.inner.send(message);
        let shown = self.shown.clone();
        tokio::spawn(async move {
            if let Err(e) = sending.await {
                tracing::warn!(server = %shown, error = %e, "could not send an elicitation answer");
            }
        });
    }

    fn answer_with(&mut self, id: RequestId, action: ElicitationAction) {
        self.respond(id, ElicitResult::new(action));
    }

    fn deliver(&mut self, id: RequestId, reply: ElicitReply) {
        // An answer for a request no longer pending was cancelled while it was on its way.
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        match reply {
            ElicitReply::Accept(answer) => match pending.form.validate(&answer) {
                Ok(()) => self.respond(
                    id,
                    ElicitResult::new(ElicitationAction::Accept)
                        .with_content(Value::Object(answer)),
                ),
                Err(reason) => {
                    tracing::warn!(
                        server = %self.shown,
                        reason,
                        "an elicitation answer did not match the requested form; declining"
                    );
                    self.answer_with(id, ElicitationAction::Decline);
                }
            },
            ElicitReply::Decline => self.answer_with(id, ElicitationAction::Decline),
            ElicitReply::Cancel => self.answer_with(id, ElicitationAction::Cancel),
        }
    }

    fn start(&mut self, id: RequestId, message: String, schema: ElicitationSchema) {
        // A set keeps each finished task's result until it is joined.
        while self.tasks.try_join_next().is_some() {}
        let flight = *self.calls.borrow();
        if flight.sub > 0 {
            // A subagent's call cannot put a dialog in front of the person.
            self.answer_with(id, ElicitationAction::Cancel);
            return;
        }
        let form = ElicitForm {
            server: self.server.clone(),
            message,
            schema,
        };
        let elicitor = self.elicitor.clone();
        let sender = self.answer_sender.clone();
        let asked = form.clone();
        let task_id = id.clone();
        let abort = self.tasks.spawn(async move {
            let reply = elicitor.elicit(asked).await;
            let _ = sender.send(Answer { id: task_id, reply });
        });
        self.pending.insert(
            id,
            Pending {
                abort,
                form,
                during_call: flight.total() > 0,
            },
        );
    }

    /// The last call ended: dialogs it caused have nothing left to answer.
    fn end_calls(&mut self) {
        let ended: Vec<RequestId> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.during_call)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ended {
            if let Some(pending) = self.pending.remove(&id) {
                pending.abort.abort();
            }
            self.answer_with(id, ElicitationAction::Cancel);
        }
    }

    /// Whether `message` was taken; a message not taken goes on to rmcp.
    fn take(&mut self, message: &ServerJsonRpcMessage) -> bool {
        match message {
            ServerJsonRpcMessage::Request(request) => {
                let ServerRequest::ElicitRequest(elicit) = &request.request else {
                    return false;
                };
                match &elicit.params {
                    ElicitRequestParams::FormElicitationParams {
                        message,
                        requested_schema,
                        ..
                    } => self.start(
                        request.id.clone(),
                        message.clone(),
                        requested_schema.clone(),
                    ),
                    ElicitRequestParams::UrlElicitationParams { .. } => {
                        tracing::warn!(
                            server = %self.shown,
                            "declined a URL-mode elicitation: opening a link on a server's say-so is not supported"
                        );
                        self.answer_with(request.id.clone(), ElicitationAction::Decline);
                    }
                    _ => self.answer_with(request.id.clone(), ElicitationAction::Decline),
                }
                true
            }
            ServerJsonRpcMessage::Notification(notification) => {
                if let ServerNotification::CancelledNotification(cancelled) =
                    &notification.notification
                    && let Some(id) = &cancelled.params.request_id
                    && let Some(pending) = self.pending.remove(id)
                {
                    pending.abort.abort();
                }
                // rmcp still sees it, so its own bookkeeping for the id stays right.
                false
            }
            _ => false,
        }
    }
}

impl<T: Transport<RoleClient>> Transport<RoleClient> for ElicitationTap<T> {
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
        loop {
            tokio::select! {
                message = self.inner.receive() => {
                    let message = message?;
                    if !self.take(&message) {
                        return Some(message);
                    }
                }
                Some(answer) = self.answers.recv() => self.deliver(answer.id, answer.reply),
                changed = self.calls.changed(), if self.calls_open => {
                    if changed.is_ok() {
                        if self.calls.borrow_and_update().total() == 0 {
                            self.end_calls();
                        }
                    } else {
                        // The tools are gone, so no call is in flight and none can start; the
                        // branch must not spin on a closed channel.
                        self.calls_open = false;
                        self.end_calls();
                    }
                }
            }
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::sync::Mutex;
    use std::time::Duration;

    use rmcp::model::{
        CancelledNotification, CancelledNotificationParam, ElicitRequest, EmptyResult,
        JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, JsonRpcVersion2_0, PingRequest,
    };
    use serde_json::json;
    use tokio::sync::Notify;

    use super::*;

    /// A scripted server: `receive` yields what the test pushes, then waits; `send` records.
    #[derive(Clone, Default)]
    struct Script {
        incoming: Arc<Mutex<VecDeque<ServerJsonRpcMessage>>>,
        wake: Arc<Notify>,
        sent: Arc<Mutex<Vec<ClientJsonRpcMessage>>>,
        hung_up: Arc<Mutex<bool>>,
    }

    impl Script {
        fn push(&self, message: ServerJsonRpcMessage) {
            self.incoming.lock().unwrap().push_back(message);
            self.wake.notify_one();
        }

        fn sent(&self) -> Vec<Value> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|m| serde_json::to_value(m).unwrap())
                .collect()
        }
    }

    impl Transport<RoleClient> for Script {
        type Error = Infallible;

        // The trait's future must be 'static, which an `async fn` borrowing `self` is not.
        #[allow(clippy::manual_async_fn)]
        fn send(
            &mut self,
            item: TxJsonRpcMessage<RoleClient>,
        ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
            let sent = self.sent.clone();
            async move {
                sent.lock().unwrap().push(item);
                Ok(())
            }
        }

        async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
            loop {
                if let Some(message) = self.incoming.lock().unwrap().pop_front() {
                    return Some(message);
                }
                if *self.hung_up.lock().unwrap() {
                    return None;
                }
                self.wake.notified().await;
            }
        }

        async fn close(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// Answers each form with the next reply in `replies` after `delay`, recording the forms.
    struct Scripted {
        replies: Mutex<VecDeque<ElicitReply>>,
        forms: Mutex<Vec<ElicitForm>>,
        started: Arc<Notify>,
        dropped: Arc<Mutex<usize>>,
        delay: Duration,
    }

    impl Scripted {
        fn new(replies: Vec<ElicitReply>) -> Arc<Self> {
            Arc::new(Scripted {
                replies: Mutex::new(replies.into()),
                forms: Mutex::new(Vec::new()),
                started: Arc::new(Notify::new()),
                dropped: Arc::new(Mutex::new(0)),
                delay: Duration::ZERO,
            })
        }

        fn slow(delay: Duration) -> Arc<Self> {
            Arc::new(Scripted {
                replies: Mutex::new(VecDeque::new()),
                forms: Mutex::new(Vec::new()),
                started: Arc::new(Notify::new()),
                dropped: Arc::new(Mutex::new(0)),
                delay,
            })
        }

        fn asked(&self) -> usize {
            self.forms.lock().unwrap().len()
        }
    }

    struct DropCount(Arc<Mutex<usize>>);

    impl Drop for DropCount {
        fn drop(&mut self) {
            *self.0.lock().unwrap() += 1;
        }
    }

    impl Elicitor for Scripted {
        fn elicit<'a>(&'a self, form: ElicitForm) -> BoxFuture<'a, ElicitReply> {
            Box::pin(async move {
                let _count = DropCount(self.dropped.clone());
                self.forms.lock().unwrap().push(form);
                self.started.notify_one();
                if !self.delay.is_zero() {
                    tokio::time::sleep(self.delay).await;
                }
                self.replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(ElicitReply::Cancel)
            })
        }
    }

    fn name_schema() -> ElicitationSchema {
        ElicitationSchema::builder()
            .required_string("name")
            .optional_integer("age", 0, 150)
            .build()
            .unwrap()
    }

    fn elicit_request(id: i64, schema: ElicitationSchema) -> ServerJsonRpcMessage {
        ServerJsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JsonRpcVersion2_0,
            id: RequestId::Number(id),
            request: ServerRequest::ElicitRequest(ElicitRequest::new(
                ElicitRequestParams::FormElicitationParams {
                    meta: None,
                    message: "Who are you?".to_string(),
                    requested_schema: schema,
                },
            )),
        })
    }

    fn ping(id: i64) -> ServerJsonRpcMessage {
        ServerJsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JsonRpcVersion2_0,
            id: RequestId::Number(id),
            request: ServerRequest::PingRequest(PingRequest::default()),
        })
    }

    fn cancelled(id: i64) -> ServerJsonRpcMessage {
        ServerJsonRpcMessage::Notification(JsonRpcNotification {
            jsonrpc: JsonRpcVersion2_0,
            notification: ServerNotification::CancelledNotification(CancelledNotification::new(
                CancelledNotificationParam::new(Some(RequestId::Number(id)), None),
            )),
        })
    }

    struct Rig {
        script: Script,
        tracker: CallTracker,
        elicitor: Arc<Scripted>,
        tap: ElicitationTap<Script>,
    }

    fn rig(elicitor: Arc<Scripted>) -> Rig {
        let script = Script::default();
        let tracker = CallTracker::new();
        let tap = ElicitationTap::new(script.clone(), "srv", "srv", elicitor.clone(), &tracker);
        Rig {
            script,
            tracker,
            elicitor,
            tap,
        }
    }

    /// Runs `receive` until the tap has nothing more to hand rmcp for `wait`.
    async fn drain(tap: &mut ElicitationTap<Script>, wait: Duration) -> Vec<Value> {
        let mut out = Vec::new();
        while let Ok(Some(message)) = tokio::time::timeout(wait, tap.receive()).await {
            out.push(serde_json::to_value(message).unwrap());
        }
        out
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    fn response(sent: &[Value], id: i64) -> Value {
        sent.iter()
            .find(|m| m["id"] == json!(id))
            .cloned()
            .unwrap_or_else(|| panic!("no response for {id} in {sent:?}"))
    }

    #[tokio::test]
    async fn a_form_request_is_taken_and_never_reaches_rmcp() {
        let mut rig = rig(Scripted::new(vec![ElicitReply::Cancel]));
        rig.script.push(elicit_request(7, name_schema()));
        let passed = drain(&mut rig.tap, Duration::from_millis(100)).await;
        assert!(passed.is_empty(), "{passed:?}");
        let forms = rig.elicitor.forms.lock().unwrap().clone();
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0].server, "srv");
        assert_eq!(forms[0].message, "Who are you?");
        assert_eq!(forms[0].schema, name_schema());
    }

    #[tokio::test]
    async fn the_answer_goes_back_with_the_requests_id() {
        let answer = Map::from_iter([("name".to_string(), json!("Ada"))]);
        let mut rig = rig(Scripted::new(vec![ElicitReply::Accept(answer)]));
        rig.script.push(elicit_request(7, name_schema()));
        drain(&mut rig.tap, Duration::from_millis(100)).await;
        settle().await;
        let sent = rig.script.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["id"], json!(7));
        assert_eq!(sent[0]["result"]["action"], json!("accept"));
        assert_eq!(sent[0]["result"]["content"], json!({"name": "Ada"}));
    }

    #[tokio::test]
    async fn cancel_and_decline_map_to_their_protocol_actions() {
        let mut rig = rig(Scripted::new(vec![
            ElicitReply::Cancel,
            ElicitReply::Decline,
        ]));
        rig.script.push(elicit_request(1, name_schema()));
        rig.script.push(elicit_request(2, name_schema()));
        drain(&mut rig.tap, Duration::from_millis(100)).await;
        settle().await;
        let sent = rig.script.sent();
        assert_eq!(response(&sent, 1)["result"]["action"], json!("cancel"));
        assert_eq!(response(&sent, 2)["result"]["action"], json!("decline"));
        assert!(response(&sent, 1)["result"].get("content").is_none());
    }

    #[tokio::test]
    async fn url_mode_is_declined_without_asking() {
        let mut rig = rig(Scripted::new(vec![]));
        rig.script
            .push(ServerJsonRpcMessage::Request(JsonRpcRequest {
                jsonrpc: JsonRpcVersion2_0,
                id: RequestId::Number(3),
                request: ServerRequest::ElicitRequest(ElicitRequest::new(
                    ElicitRequestParams::UrlElicitationParams {
                        meta: None,
                        message: "Sign in".to_string(),
                        url: "https://example.com/auth".to_string(),
                        elicitation_id: "e1".to_string(),
                    },
                )),
            }));
        let passed = drain(&mut rig.tap, Duration::from_millis(100)).await;
        settle().await;
        assert!(passed.is_empty());
        assert_eq!(rig.elicitor.asked(), 0);
        assert_eq!(
            response(&rig.script.sent(), 3)["result"]["action"],
            json!("decline")
        );
    }

    #[tokio::test]
    async fn a_cancelled_notification_aborts_the_task_and_sends_no_response() {
        let elicitor = Scripted::slow(Duration::from_secs(3600));
        let mut rig = rig(elicitor);
        rig.script.push(elicit_request(9, name_schema()));
        drain(&mut rig.tap, Duration::from_millis(100)).await;
        assert_eq!(rig.elicitor.asked(), 1);
        rig.script.push(cancelled(9));
        let passed = drain(&mut rig.tap, Duration::from_millis(100)).await;
        settle().await;
        // The notification still reaches rmcp; the request does not.
        assert_eq!(passed.len(), 1);
        assert_eq!(passed[0]["method"], json!("notifications/cancelled"));
        assert!(rig.script.sent().is_empty());
        assert_eq!(
            *rig.elicitor.dropped.lock().unwrap(),
            1,
            "the dialog future was dropped"
        );
    }

    #[tokio::test]
    async fn dropping_the_tap_aborts_a_pending_elicitation() {
        let elicitor = Scripted::slow(Duration::from_secs(3600));
        let mut rig = rig(elicitor.clone());
        rig.script.push(elicit_request(1, name_schema()));
        drain(&mut rig.tap, Duration::from_millis(100)).await;
        assert_eq!(*elicitor.dropped.lock().unwrap(), 0);
        drop(rig.tap);
        settle().await;
        assert_eq!(*elicitor.dropped.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn other_messages_pass_through_untouched() {
        let mut rig = rig(Scripted::new(vec![]));
        let ping = ping(4);
        let list_changed = ServerJsonRpcMessage::Notification(JsonRpcNotification {
            jsonrpc: JsonRpcVersion2_0,
            notification: ServerNotification::ToolListChangedNotification(
                rmcp::model::ToolListChangedNotification::default(),
            ),
        });
        let empty = ServerJsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: JsonRpcVersion2_0,
            id: RequestId::Number(6),
            result: rmcp::model::ServerResult::EmptyResult(EmptyResult {}),
        });
        let expected: Vec<Value> = [&ping, &list_changed, &empty]
            .iter()
            .map(|m| serde_json::to_value(m).unwrap())
            .collect();
        rig.script.push(ping);
        rig.script.push(list_changed);
        rig.script.push(empty);
        assert_eq!(
            drain(&mut rig.tap, Duration::from_millis(100)).await,
            expected
        );
        assert!(rig.script.sent().is_empty());
    }

    #[tokio::test]
    async fn an_answer_that_breaks_the_schema_is_declined() {
        // `name` is required and `age` is out of range.
        let bad = Map::from_iter([("age".to_string(), json!(500))]);
        let mut rig = rig(Scripted::new(vec![ElicitReply::Accept(bad)]));
        rig.script.push(elicit_request(2, name_schema()));
        drain(&mut rig.tap, Duration::from_millis(100)).await;
        settle().await;
        let sent = rig.script.sent();
        assert_eq!(response(&sent, 2)["result"]["action"], json!("decline"));
        assert!(response(&sent, 2)["result"].get("content").is_none());
    }

    #[tokio::test]
    async fn a_form_while_a_subagents_call_is_in_flight_is_cancelled_without_asking() {
        let mut rig = rig(Scripted::new(vec![]));
        let _call = rig.tracker.enter(false);
        rig.script.push(elicit_request(1, name_schema()));
        drain(&mut rig.tap, Duration::from_millis(100)).await;
        settle().await;
        assert_eq!(rig.elicitor.asked(), 0);
        assert_eq!(
            response(&rig.script.sent(), 1)["result"]["action"],
            json!("cancel")
        );
    }

    #[tokio::test]
    async fn a_form_while_the_main_agents_call_is_in_flight_is_asked() {
        let answer = Map::from_iter([("name".to_string(), json!("Ada"))]);
        let mut rig = rig(Scripted::new(vec![ElicitReply::Accept(answer)]));
        let _call = rig.tracker.enter(true);
        rig.script.push(elicit_request(1, name_schema()));
        drain(&mut rig.tap, Duration::from_millis(100)).await;
        settle().await;
        assert_eq!(rig.elicitor.asked(), 1);
        assert_eq!(
            response(&rig.script.sent(), 1)["result"]["action"],
            json!("accept")
        );
    }

    #[tokio::test]
    async fn when_the_last_call_ends_its_pending_elicitation_is_cancelled() {
        let mut rig = rig(Scripted::slow(Duration::from_secs(3600)));
        let call = rig.tracker.enter(true);
        rig.script.push(elicit_request(1, name_schema()));
        drain(&mut rig.tap, Duration::from_millis(100)).await;
        assert_eq!(rig.elicitor.asked(), 1);
        assert!(rig.script.sent().is_empty());
        drop(call);
        drain(&mut rig.tap, Duration::from_millis(100)).await;
        settle().await;
        assert_eq!(
            response(&rig.script.sent(), 1)["result"]["action"],
            json!("cancel")
        );
        assert_eq!(
            *rig.elicitor.dropped.lock().unwrap(),
            1,
            "its task was aborted"
        );
    }

    #[tokio::test]
    async fn an_elicitation_that_arrived_with_no_call_in_flight_stays_open() {
        let mut rig = rig(Scripted::slow(Duration::from_secs(3600)));
        rig.script.push(elicit_request(1, name_schema()));
        drain(&mut rig.tap, Duration::from_millis(100)).await;
        let call = rig.tracker.enter(true);
        drop(call);
        drain(&mut rig.tap, Duration::from_millis(100)).await;
        settle().await;
        assert!(rig.script.sent().is_empty());
        assert_eq!(*rig.elicitor.dropped.lock().unwrap(), 0);
    }

    #[test]
    fn a_tracker_counts_main_and_sub_calls_apart() {
        let tracker = CallTracker::new();
        let rx = tracker.subscribe();
        let a = tracker.enter(true);
        let b = tracker.enter(false);
        let c = tracker.enter(false);
        assert_eq!(*rx.borrow(), InFlight { main: 1, sub: 2 });
        drop((a, b));
        assert_eq!(*rx.borrow(), InFlight { main: 0, sub: 1 });
        drop(c);
        assert_eq!(*rx.borrow(), InFlight::default());
    }

    fn form(schema: ElicitationSchema) -> ElicitForm {
        ElicitForm {
            server: "srv".into(),
            message: "m".into(),
            schema,
        }
    }

    fn answer(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn validation_checks_required_fields_types_and_ranges() {
        let f = form(
            ElicitationSchema::builder()
                .required_string_with("name", |s| s.length(2, 4))
                .optional_integer("age", 1, 10)
                .optional_number("ratio", 0.0, 1.0)
                .optional_bool("ok", false)
                .build()
                .unwrap(),
        );
        assert!(f.validate(&answer(&[("name", json!("Ada"))])).is_ok());
        let bad = |pairs: &[(&str, Value)], needle: &str| {
            let error = f.validate(&answer(pairs)).unwrap_err();
            assert!(error.contains(needle), "{error}");
        };
        bad(&[], "name is required");
        bad(&[("name", json!("A"))], "at least 2");
        bad(&[("name", json!("Adaaa"))], "at most 4");
        bad(&[("name", json!(5))], "must be text");
        bad(&[("name", json!("Ada")), ("age", json!(11))], "at most 10");
        bad(&[("name", json!("Ada")), ("age", json!(0))], "at least 1");
        bad(
            &[("name", json!("Ada")), ("age", json!(2.5))],
            "whole number",
        );
        bad(
            &[("name", json!("Ada")), ("ratio", json!(1.5))],
            "at most 1",
        );
        bad(
            &[("name", json!("Ada")), ("ok", json!("yes"))],
            "true or false",
        );
        bad(
            &[("name", json!("Ada")), ("extra", json!(1))],
            "not a field",
        );
    }

    #[test]
    fn validation_checks_string_formats() {
        let f = form(
            ElicitationSchema::builder()
                .optional_string_with("email", |s| s.format(StringFormat::Email))
                .optional_string_with("link", |s| s.format(StringFormat::Uri))
                .optional_string_with("day", |s| s.format(StringFormat::Date))
                .optional_string_with("at", |s| s.format(StringFormat::DateTime))
                .build()
                .unwrap(),
        );
        for (key, good, bad) in [
            ("email", "ada@example.com", "ada@example"),
            ("link", "https://example.com/x", "not a uri"),
            ("day", "2024-02-29", "2023-02-29"),
            ("at", "2024-05-01T12:30:00Z", "2024-05-01 12:30"),
        ] {
            assert!(f.validate(&answer(&[(key, json!(good))])).is_ok(), "{good}");
            assert!(f.validate(&answer(&[(key, json!(bad))])).is_err(), "{bad}");
        }
        assert!(
            f.validate(&answer(&[("at", json!("2024-05-01T12:30:00.5+02:00"))]))
                .is_ok()
        );
        assert!(
            f.validate(&answer(&[("at", json!("2024-05-01T25:30:00Z"))]))
                .is_err()
        );
        assert!(
            f.validate(&answer(&[("day", json!("2024-13-01"))]))
                .is_err()
        );
    }

    #[test]
    fn validation_checks_enum_membership_and_multi_select_bounds() {
        use rmcp::model::{ConstTitle, EnumSchema, TitledItems, TitledMultiSelectEnumSchema};
        let single = EnumSchema::builder(vec!["a".into(), "b".into()]).build();
        let multi = EnumSchema::Multi(MultiSelectEnumSchema::Titled(
            TitledMultiSelectEnumSchema::new(TitledItems::new(vec![
                ConstTitle::new("x", "Ex"),
                ConstTitle::new("y", "Why"),
                ConstTitle::new("z", "Zed"),
            ]))
            .with_min_items(1)
            .with_max_items(2),
        ));
        let f = form(
            ElicitationSchema::builder()
                .optional_enum_schema("pick", single)
                .optional_enum_schema("many", multi)
                .build()
                .unwrap(),
        );
        assert!(f.validate(&answer(&[("pick", json!("a"))])).is_ok());
        assert!(f.validate(&answer(&[("pick", json!("c"))])).is_err());
        assert!(f.validate(&answer(&[("many", json!(["x", "y"]))])).is_ok());
        assert!(f.validate(&answer(&[("many", json!([]))])).is_err());
        assert!(
            f.validate(&answer(&[("many", json!(["x", "y", "z"]))]))
                .is_err()
        );
        assert!(f.validate(&answer(&[("many", json!(["w"]))])).is_err());
        assert!(f.validate(&answer(&[("many", json!("x"))])).is_err());
        let fields = f.fields();
        let FieldKind::MultiSelect { options, .. } = &fields[0].kind else {
            panic!("{fields:?}");
        };
        assert_eq!(options[0].title, "Ex");
    }

    #[test]
    fn fields_follow_property_order_and_carry_titles_and_required_marks() {
        let mut schema = ElicitationSchema::builder()
            .required_string("b")
            .optional_string("a")
            .build()
            .unwrap();
        schema.property_order = Some(vec!["b".into(), "a".into()]);
        let fields = form(schema.clone()).fields();
        assert_eq!(
            fields.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert!(fields[0].required && !fields[1].required);
        schema.property_order = None;
        let fields = form(schema).fields();
        assert_eq!(
            fields.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn typed_text_becomes_a_value_or_an_error() {
        let int = FieldKind::Integer {
            minimum: None,
            maximum: None,
            default: None,
        };
        let num = FieldKind::Number {
            minimum: None,
            maximum: None,
            default: None,
        };
        assert_eq!(int.value_of_text(" 42 "), Ok(Some(json!(42))));
        assert_eq!(int.value_of_text(""), Ok(None));
        assert!(int.value_of_text("4.5").is_err());
        assert_eq!(num.value_of_text("2.5"), Ok(Some(json!(2.5))));
        assert!(num.value_of_text("abc").is_err());
        assert!(num.value_of_text("NaN").is_err());
    }
}
