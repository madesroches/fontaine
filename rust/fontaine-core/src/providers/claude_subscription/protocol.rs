//! The two translations this adapter makes, as pure functions: a Messages request body into the
//! stream-json frames, private settings and tool manifest the CLI takes, and the CLI's stdout
//! events back into the Messages stream frames rig's decoder reads.
//!
//! Nothing here touches a process, so every refusal and every terminal-state rule is a unit
//! test.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use rig_core::error::ProviderError;
use rig_core::wire::WireFrame;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::cli::limit_error;
use super::usage::{Usage, parse_rate_limit};
use super::{ClaudeError, ErrorKind, claude_error};

/// What the CLI calls a tool of the `fontaine` MCP server.
pub const TOOL_PREFIX: &str = "mcp__fontaine__";

/// The CLI rejects tool names over 64 characters.
const MAX_ALIAS: usize = 64 - TOOL_PREFIX.len();

/// The only built-in the CLI may still list: control flow, never a Fontaine tool call.
const TOLERATED_BUILTIN: &str = "EndConversation";

const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

fn unsupported(message: impl Into<String>) -> ClaudeError {
    ClaudeError::new(ErrorKind::Unsupported, message)
}

/// A failure as the readers return it: boxed, because a provider error is large.
pub type Failure = Box<ProviderError>;

fn protocol(message: &str) -> Failure {
    ClaudeError::new(ErrorKind::Protocol, message).boxed()
}

const SHOWN_ENTRIES: usize = 5;
const ENTRY_CHARS: usize = 80;

/// Plugins compiled into the CLI itself, which it lists even with every setting source off. Both
/// markers are required so a user plugin carrying one of them is not mistaken for a bundled one.
fn is_bundled(plugin: &Value) -> bool {
    plugin.get("path").and_then(Value::as_str) == Some("builtin")
        && plugin
            .get("source")
            .and_then(Value::as_str)
            .is_some_and(|source| source.ends_with("@builtin"))
}

/// Names the offending entries so the user can find what the CLI read; the entries come from the
/// CLI, so their number and length are capped.
fn customization_error(kind: &str, entries: &[&Value]) -> Failure {
    let render = |entry: &Value| -> String {
        let text = entry
            .get("source")
            .and_then(Value::as_str)
            .or_else(|| entry.get("name").and_then(Value::as_str))
            .or_else(|| entry.as_str())
            .unwrap_or("?");
        text.chars().take(ENTRY_CHARS).collect()
    };
    let mut names: Vec<String> = entries
        .iter()
        .take(SHOWN_ENTRIES)
        .map(|entry| render(entry))
        .collect();
    if entries.len() > SHOWN_ENTRIES {
        names.push(format!("and {} more", entries.len() - SHOWN_ENTRIES));
    }
    let names = names.join(", ");
    let accepted = if kind == "plugins" {
        "only the CLI's builtin plugins are accepted"
    } else {
        "no skills are accepted"
    };
    protocol(&format!(
        "the Claude CLI loaded {kind} Fontaine did not ask for: {names}; {accepted}. The CLI \
         loaded them even with every setting source switched off; look at the CLI's \
         managed/policy settings or the environment Fontaine passes through to it"
    ))
}

/// Tool names as the CLI sees them: `mcp__fontaine__<name>`, with names that need length or
/// character normalization replaced by a stable, reversible alias.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Aliases {
    /// Original name to the part after the prefix.
    short: BTreeMap<String, String>,
    /// Full CLI name back to the original name.
    original: BTreeMap<String, String>,
}

impl Aliases {
    pub fn new<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut aliases = Aliases::default();
        for name in names {
            let short = alias_part(name);
            aliases
                .original
                .insert(format!("{TOOL_PREFIX}{short}"), name.to_string());
            aliases.short.insert(name.to_string(), short);
        }
        aliases
    }

    /// The CLI's name for `name`.
    pub fn full(&self, name: &str) -> Option<String> {
        self.short
            .get(name)
            .map(|short| format!("{TOOL_PREFIX}{short}"))
    }

    /// The name the MCP inventory lists `name` under.
    pub fn short(&self, name: &str) -> Option<&str> {
        self.short.get(name).map(String::as_str)
    }

    /// The Fontaine name behind a CLI name.
    pub fn restore(&self, full: &str) -> Option<&str> {
        self.original.get(full).map(String::as_str)
    }
}

fn alias_part(name: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    if !name.is_empty() && name.len() <= MAX_ALIAS && name.chars().all(plain) {
        return name.to_string();
    }
    let digest = Sha256::digest(name.as_bytes());
    let hash: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    let stem: String = name
        .chars()
        .map(|c| if plain(c) { c } else { '_' })
        .take(MAX_ALIAS - 9)
        .collect();
    format!("{stem}_{hash}")
}

/// One stream-json input frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub value: Value,
    /// Whether this is the frame that asks for a response. Every earlier frame is history the
    /// CLI acknowledges and does not answer.
    pub query: bool,
}

/// Everything one CLI process needs from a Messages request.
#[derive(Clone, Debug, PartialEq)]
pub struct Translated {
    pub system: String,
    pub frames: Vec<Frame>,
    pub settings: Value,
    /// The inert MCP inventory: each tool's name, description and input schema.
    pub manifest: Vec<Value>,
    pub effort: Option<String>,
    pub aliases: Aliases,
    pub session_id: String,
}

/// What a session id is derived from besides the request.
pub struct SessionSeed<'a> {
    pub session: &'a str,
    pub namespace: &'a str,
    pub canonical: &'a str,
}

/// A stable UUID from the Fontaine session, the caller, the model, the system prompt and the
/// first frame, so an unchanged prefix keeps its id and nothing else shares one.
pub fn derive_session_id(seed: &SessionSeed<'_>, system: &str, first: &Value) -> String {
    let mut hash = Sha256::new();
    for part in [
        seed.session,
        seed.namespace,
        seed.canonical,
        system,
        &first.to_string(),
    ] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    let digest = hash.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}

const KNOWN_FIELDS: [&str; 13] = [
    "model",
    "system",
    "messages",
    "tools",
    "tool_choice",
    "max_tokens",
    "temperature",
    "top_p",
    "top_k",
    "stop_sequences",
    "thinking",
    "output_config",
    "stream",
];

/// Translates an encoded Messages body. `betas` is the request's `anthropic-beta` header, the
/// one header that is not a credential or an identity.
pub fn translate(
    body: &[u8],
    betas: Option<&str>,
    seed: &SessionSeed<'_>,
) -> Result<Translated, ClaudeError> {
    let body: Value = serde_json::from_slice(body)
        .map_err(|_| unsupported("the request body is not JSON the Claude CLI adapter can read"))?;
    let object = body
        .as_object()
        .ok_or_else(|| unsupported("the request body is not an object"))?;
    for key in object.keys() {
        if !KNOWN_FIELDS.contains(&key.as_str()) {
            return Err(unsupported(format!(
                "the request field `{key}` cannot be sent through the Claude CLI"
            )));
        }
    }

    let tools = tool_definitions(object.get("tools"))?;
    let aliases = Aliases::new(tools.iter().map(|(name, _)| name.as_str()));
    let system = system_text(object.get("system"))?;
    // A fork replays the parent's calls to tools the child's request omits; they still need CLI
    // names, but only the request's own tools are callable in the response.
    let history = Aliases::new(
        tools
            .iter()
            .map(|(name, _)| name.as_str())
            .chain(history_tool_names(object.get("messages"))),
    );
    let frames = frames_of(object.get("messages"), &history)?;
    let session_id = derive_session_id(seed, &system, &frames[0].value);

    let mut extra = Map::new();
    for key in [
        "max_tokens",
        "temperature",
        "top_p",
        "top_k",
        "stop_sequences",
        "thinking",
    ] {
        if let Some(value) = object.get(key) {
            extra.insert(key.to_string(), value.clone());
        }
    }
    let mut effort = None;
    if let Some(config) = object.get("output_config") {
        let mut config = config
            .as_object()
            .cloned()
            .ok_or_else(|| unsupported("`output_config` is not an object"))?;
        if let Some(level) = config.remove("effort") {
            let level = level
                .as_str()
                .filter(|level| EFFORTS.contains(level))
                .ok_or_else(|| {
                    unsupported("the requested effort level is not one the CLI takes")
                })?;
            effort = Some(level.to_string());
        }
        if !config.is_empty() {
            extra.insert("output_config".to_string(), Value::Object(config));
        }
    }
    let mut manifest = Vec::new();
    let mut listed = Vec::new();
    for (name, definition) in &tools {
        let full = aliases.full(name).expect("every tool was aliased");
        let short = aliases.short(name).expect("every tool was aliased");
        let mut aliased = definition.clone();
        aliased.insert("name".to_string(), Value::String(full));
        aliased.remove("cache_control");
        listed.push(Value::Object(aliased));
        manifest.push(json!({
            "name": short,
            "description": definition.get("description").cloned().unwrap_or(Value::Null),
            "inputSchema": definition.get("input_schema").cloned().unwrap_or(Value::Null),
        }));
    }
    if !listed.is_empty() {
        extra.insert("tools".to_string(), Value::Array(listed));
    }
    if let Some(choice) = object.get("tool_choice") {
        extra.insert("tool_choice".to_string(), tool_choice(choice, &aliases)?);
    }

    let mut settings = super::cli::base_settings();
    let env = settings
        .get_mut("env")
        .and_then(Value::as_object_mut)
        .expect("the base settings carry an env object");
    env.insert(
        "CLAUDE_CODE_EXTRA_BODY".to_string(),
        Value::String(Value::Object(extra).to_string()),
    );
    if let Some(betas) = betas.filter(|b| !b.is_empty()) {
        env.insert(
            "ANTHROPIC_BETAS".to_string(),
            Value::String(betas.to_string()),
        );
    }

    Ok(Translated {
        system,
        frames,
        settings,
        manifest,
        effort,
        aliases,
        session_id,
    })
}

type Definitions = Vec<(String, Map<String, Value>)>;

fn tool_definitions(tools: Option<&Value>) -> Result<Definitions, ClaudeError> {
    let Some(tools) = tools else {
        return Ok(Vec::new());
    };
    let tools = tools
        .as_array()
        .ok_or_else(|| unsupported("`tools` is not a list"))?;
    let mut out: Definitions = Vec::new();
    for tool in tools {
        let tool = tool
            .as_object()
            .ok_or_else(|| unsupported("a tool definition is not an object"))?;
        let custom = tool
            .get("type")
            .is_none_or(|kind| kind.as_str() == Some("custom"));
        if !custom || !tool.contains_key("input_schema") {
            return Err(unsupported(
                "a hosted tool definition cannot be sent through the Claude CLI",
            ));
        }
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| unsupported("a tool definition has no name"))?;
        if out.iter().any(|(existing, _)| existing == name) {
            return Err(unsupported("two tool definitions share a name"));
        }
        out.push((name.to_string(), tool.clone()));
    }
    Ok(out)
}

fn tool_choice(choice: &Value, aliases: &Aliases) -> Result<Value, ClaudeError> {
    let mut choice = choice
        .as_object()
        .cloned()
        .ok_or_else(|| unsupported("`tool_choice` is not an object"))?;
    if let Some(name) = choice.get("name").and_then(Value::as_str) {
        let full = aliases
            .full(name)
            .ok_or_else(|| unsupported("`tool_choice` names a tool the request does not define"))?;
        choice.insert("name".to_string(), Value::String(full));
    }
    Ok(Value::Object(choice))
}

fn system_text(system: Option<&Value>) -> Result<String, ClaudeError> {
    match system {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Array(blocks)) => {
            let mut parts = Vec::new();
            for block in blocks {
                match (block.get("type").and_then(Value::as_str), block.get("text")) {
                    (Some("text"), Some(Value::String(text))) => parts.push(text.as_str()),
                    _ => {
                        return Err(unsupported(
                            "the system prompt holds a block that is not text",
                        ));
                    }
                }
            }
            Ok(parts.join("\n\n"))
        }
        Some(_) => Err(unsupported("the system prompt is neither text nor blocks")),
    }
}

fn history_tool_names(messages: Option<&Value>) -> impl Iterator<Item = &str> {
    messages
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .filter_map(|message| message.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|block| block.get("name").and_then(Value::as_str))
}

fn frames_of(messages: Option<&Value>, aliases: &Aliases) -> Result<Vec<Frame>, ClaudeError> {
    let messages = messages
        .and_then(Value::as_array)
        .filter(|messages| !messages.is_empty())
        .ok_or_else(|| unsupported("the request has no messages"))?;
    let mut frames = Vec::new();
    let last = messages.len() - 1;
    for (index, message) in messages.iter().enumerate() {
        let role = message.get("role").and_then(Value::as_str);
        let content = message.get("content");
        let frame = match role {
            Some("user") => {
                let content = user_content(content)?;
                let mut value = json!({
                    "type": "user",
                    "message": {"role": "user", "content": content},
                    "parent_tool_use_id": null,
                });
                if index != last {
                    value["shouldQuery"] = Value::Bool(false);
                }
                Frame {
                    value,
                    query: index == last,
                }
            }
            Some("assistant") => {
                if index == last {
                    return Err(unsupported(
                        "a request ending in an assistant message (prefill) cannot be sent through the Claude CLI",
                    ));
                }
                Frame {
                    value: json!({
                        "type": "assistant",
                        "message": {"role": "assistant", "content": assistant_content(content, aliases)?},
                        "parent_tool_use_id": null,
                    }),
                    query: false,
                }
            }
            _ => {
                return Err(unsupported(
                    "a message with a role other than user or assistant cannot be replayed through the Claude CLI",
                ));
            }
        };
        frames.push(frame);
    }
    Ok(frames)
}

fn blocks_of(content: Option<&Value>) -> Result<Vec<Value>, ClaudeError> {
    match content {
        Some(Value::String(text)) => Ok(vec![json!({"type": "text", "text": text})]),
        Some(Value::Array(blocks)) if !blocks.is_empty() => Ok(blocks.clone()),
        _ => Err(unsupported("a message has no content")),
    }
}

fn user_content(content: Option<&Value>) -> Result<Value, ClaudeError> {
    if let Some(Value::String(text)) = content {
        if text.is_empty() {
            return Err(unsupported("a message has no content"));
        }
        return Ok(Value::String(text.clone()));
    }
    let mut blocks = blocks_of(content)?;
    for block in &mut blocks {
        let kind = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(kind, "text" | "image" | "document" | "tool_result") {
            return Err(unsupported(
                "a user message holds content the Claude CLI cannot replay without loss",
            ));
        }
        if let Some(block) = block.as_object_mut() {
            block.remove("cache_control");
        }
    }
    Ok(Value::Array(blocks))
}

fn assistant_content(content: Option<&Value>, aliases: &Aliases) -> Result<Value, ClaudeError> {
    let mut blocks = blocks_of(content)?;
    for block in &mut blocks {
        let kind = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let kind = kind.as_str();
        if !matches!(kind, "text" | "thinking" | "redacted_thinking" | "tool_use") {
            return Err(unsupported(
                "an assistant message holds content the Claude CLI cannot replay without loss",
            ));
        }
        let Some(object) = block.as_object_mut() else {
            continue;
        };
        object.remove("cache_control");
        if kind == "tool_use" {
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let full = aliases.full(name).ok_or_else(|| {
                unsupported("the history calls a tool the request does not define")
            })?;
            object.insert("name".to_string(), Value::String(full));
        }
    }
    Ok(Value::Array(blocks))
}

/// What a response must match: the models `system/init` may name and the tools the request
/// defined.
#[derive(Clone, Debug)]
pub struct Expect {
    pub models: Vec<String>,
    pub aliases: Aliases,
}

/// What the reader is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The zero-turn result of a history frame.
    Ack,
    /// The one response to the final frame.
    Query,
}

/// What one stdout line meant.
#[derive(Debug)]
pub enum Event {
    Nothing,
    /// A Messages stream event to hand to rig's decoder.
    Frame(WireFrame),
    /// A history frame was acknowledged.
    Ack,
    /// One complete response and its final result were validated.
    Complete,
}

/// A native error category the CLI attached to its assistant message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Native {
    Auth,
    Limit,
    Server,
    Invalid,
}

/// One tool call the response announced, as the CLI named it.
struct Call {
    cli_name: String,
    denied: bool,
    echoed: bool,
}

/// The most accepted-event labels the trail keeps.
const TRAIL_ENTRIES: usize = 64;

/// A name from the CLI's output is shown only when it is plain, so an error built from it stays
/// text made of names rather than raw output.
fn safe_name(name: Option<&str>) -> String {
    match name {
        None => "(none)".to_string(),
        Some(name)
            if (1..=64).contains(&name.len())
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) =>
        {
            name.to_string()
        }
        Some(_) => "?".to_string(),
    }
}

/// The event a stdout line carries, as a name safe to show: `type`, plus `/subtype` for a
/// system event or a result and `/<event type>` for a stream event.
fn label_of(value: &Value) -> String {
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);
    let kind = text(value.get("type"));
    let mut label = safe_name(kind.as_deref());
    let inner = match kind.as_deref() {
        Some("system" | "result") => Some(text(value.get("subtype"))),
        Some("stream_event") => Some(text(value.get("event").and_then(|e| e.get("type")))),
        _ => None,
    };
    if let Some(inner) = inner {
        label.push('/');
        label.push_str(&safe_name(inner.as_deref()));
    }
    label
}

pub struct Reader {
    expect: Expect,
    calls: BTreeMap<String, Call>,
    trail: VecDeque<(String, u32)>,
    trail_cut: bool,
    phase: Phase,
    init_seen: bool,
    messages: u32,
    stop_reason: Option<String>,
    /// The terminal `message_delta`, held until the response is validated and the process
    /// stopped: rig ends its read at it, so releasing it earlier would end the read before the
    /// stop and the result were checked.
    terminal: Option<WireFrame>,
    stopped: bool,
    result_seen: bool,
    open: BTreeSet<usize>,
    suppressed: BTreeSet<usize>,
    native: Option<Native>,
    streamed_output_tokens: Option<u64>,
    usage: Option<Arc<Usage>>,
}

impl Reader {
    pub fn new(expect: Expect, usage: Option<Arc<Usage>>) -> Self {
        Reader {
            expect,
            usage,
            calls: BTreeMap::new(),
            trail: VecDeque::new(),
            trail_cut: false,
            phase: Phase::Ack,
            init_seen: false,
            messages: 0,
            stop_reason: None,
            terminal: None,
            stopped: false,
            result_seen: false,
            open: BTreeSet::new(),
            suppressed: BTreeSet::new(),
            native: None,
            streamed_output_tokens: None,
        }
    }

    pub fn expect_ack(&mut self) {
        self.phase = Phase::Ack;
    }

    pub fn expect_response(&mut self) {
        self.phase = Phase::Query;
    }

    /// The held terminal delta, to be released after the process is stopped.
    pub fn release(&mut self) -> Option<WireFrame> {
        self.terminal.take()
    }

    /// The failure category the CLI reported before it exited, if any.
    pub(crate) fn native(&self) -> Option<&Native> {
        self.native.as_ref()
    }

    /// The labels of the events accepted so far, repeats collapsed, oldest cut first.
    pub fn trail(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.trail_cut {
            parts.push("…".to_string());
        }
        parts.extend(self.trail.iter().map(|(label, n)| {
            if *n > 1 {
                format!("{label} ×{n}")
            } else {
                label.clone()
            }
        }));
        parts.join(" → ")
    }

    fn remember(&mut self, label: String) {
        match self.trail.back_mut() {
            Some((last, n)) if *last == label => *n += 1,
            _ => {
                self.trail.push_back((label, 1));
                if self.trail.len() > TRAIL_ENTRIES {
                    self.trail.pop_front();
                    self.trail_cut = true;
                }
            }
        }
    }

    pub fn accept(&mut self, line: &[u8]) -> Result<Event, Failure> {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return Err(ClaudeError::new(
                ErrorKind::Protocol,
                "the Claude CLI sent a line that is not JSON",
            )
            .with_detail(" (event: (not JSON))")
            .boxed());
        };
        let label = label_of(&value);
        match self.dispatch(&value) {
            Ok(event) => {
                self.remember(label);
                Ok(event)
            }
            Err(error) => Err(match claude_error(&error) {
                Some(typed) if typed.kind == ErrorKind::Protocol => typed
                    .clone()
                    .with_detail(&format!(" (event: {label})"))
                    .boxed(),
                _ => error,
            }),
        }
    }

    fn dispatch(&mut self, value: &Value) -> Result<Event, Failure> {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "system" => self.system(value),
            "stream_event" => self.stream_event(value),
            "assistant" => {
                if let Some(category) = value.get("error").and_then(Value::as_str) {
                    self.native = Some(match category {
                        "authentication_failed" => Native::Auth,
                        "rate_limit" | "billing_error" => Native::Limit,
                        "server_error" | "overloaded" => Native::Server,
                        _ => Native::Invalid,
                    });
                } else if let Some(category) = value
                    .get("message")
                    .and_then(|m| m.get("error"))
                    .and_then(Value::as_str)
                {
                    self.native = Some(
                        if category.contains("rate") || category.contains("billing") {
                            Native::Limit
                        } else if category.contains("auth") {
                            Native::Auth
                        } else {
                            Native::Server
                        },
                    );
                }
                Ok(Event::Nothing)
            }
            "result" => self.result(value),
            "user" => self.denial_echo(value),
            // The status is ignored: a mid-response extra-usage switch is invisible to Fontaine and failing here would not prevent a charge.
            "rate_limit_event" => {
                if let Some(usage) = &self.usage {
                    let quota = parse_rate_limit(value);
                    // A sparse frame must not erase fuller data.
                    if !quota.windows.is_empty() {
                        usage.record(quota);
                    }
                }
                Ok(Event::Nothing)
            }
            "keep_alive" => Ok(Event::Nothing),
            "prompt_suggestion" => Err(protocol(
                "the Claude CLI sent an unsolicited prompt suggestion",
            )),
            _ => Err(protocol(
                "the Claude CLI sent an event this adapter does not accept",
            )),
        }
    }

    fn system(&mut self, value: &Value) -> Result<Event, Failure> {
        match value.get("subtype").and_then(Value::as_str) {
            Some("init") => {
                self.validate_init(value)?;
                self.init_seen = true;
                Ok(Event::Nothing)
            }
            Some("status") => Ok(Event::Nothing),
            Some("api_retry") => Err(protocol("the Claude CLI retried the request on its own")),
            Some("permission_denied") => self.permission_denied(value),
            Some("thinking_tokens") => {
                if self.phase == Phase::Ack || !self.init_seen {
                    return Err(protocol(
                        "the Claude CLI sent a thinking estimate outside a response",
                    ));
                }
                Ok(Event::Nothing)
            }
            _ => Err(protocol(
                "the Claude CLI sent a system event this adapter does not accept",
            )),
        }
    }

    /// The CLI's report that it denied a call: accepted only for a call this response announced,
    /// under the name it announced, once. Its message is never read.
    fn permission_denied(&mut self, value: &Value) -> Result<Event, Failure> {
        let call = value
            .get("tool_use_id")
            .and_then(Value::as_str)
            .and_then(|id| self.calls.get_mut(id))
            .filter(|call| {
                !call.denied
                    && value.get("tool_name").and_then(Value::as_str) == Some(&call.cli_name)
            })
            .ok_or_else(|| {
                protocol("the Claude CLI reported a denial for no call of this response")
            })?;
        call.denied = true;
        Ok(Event::Nothing)
    }

    fn validate_init(&self, init: &Value) -> Result<(), Failure> {
        let named = init.get("model").and_then(Value::as_str);
        let model_ok = named.is_none_or(|model| {
            let model = model.split('[').next().unwrap_or(model);
            self.expect.models.iter().any(|m| m == model)
        });
        if !model_ok {
            return Err(protocol(
                "the Claude CLI selected a model other than the requested one",
            ));
        }
        if let Some(tools) = init.get("tools").and_then(Value::as_array) {
            for tool in tools {
                let name = tool.as_str().unwrap_or_default();
                let ours = self.expect.aliases.restore(name).is_some();
                if !(ours || name == TOLERATED_BUILTIN) {
                    return Err(protocol(
                        "the Claude CLI offers a tool Fontaine did not define",
                    ));
                }
            }
        }
        let servers = init
            .get("mcp_servers")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let one_inert = servers.len() == 1
            && servers[0].get("name").and_then(Value::as_str) == Some("fontaine")
            && matches!(
                servers[0].get("status").and_then(Value::as_str),
                Some("connected" | "pending")
            );
        if !one_inert {
            return Err(protocol(
                "the Claude CLI did not start exactly the inert tool inventory",
            ));
        }
        for key in ["plugins", "skills"] {
            let unwanted: Vec<&Value> = init
                .get(key)
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .filter(|entry| key != "plugins" || !is_bundled(entry))
                .collect();
            if !unwanted.is_empty() {
                return Err(customization_error(key, &unwanted));
            }
        }
        Ok(())
    }

    fn stream_event(&mut self, value: &Value) -> Result<Event, Failure> {
        if self.phase == Phase::Ack {
            return Err(protocol("the Claude CLI answered a history frame"));
        }
        if !self.init_seen {
            return Err(protocol(
                "the Claude CLI streamed before it reported its setup",
            ));
        }
        if !value.get("parent_tool_use_id").is_none_or(Value::is_null) {
            return Err(protocol("the Claude CLI ran a native subagent"));
        }
        let event = value
            .get("event")
            .filter(|event| event.is_object())
            .ok_or_else(|| protocol("a stream event has no event"))?;
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if self.terminal.is_some() && !matches!(kind, "message_stop" | "ping") {
            return Err(protocol(
                "the Claude CLI sent output after the response ended",
            ));
        }
        let index = event
            .get("index")
            .and_then(Value::as_u64)
            .map(|index| index as usize);
        match kind {
            "message_start" => {
                self.messages += 1;
                if self.messages > 1 {
                    return Err(protocol("the Claude CLI sent a second response"));
                }
                let model = event
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str);
                if model.is_some_and(|model| !self.expect.models.iter().any(|m| m == model)) {
                    return Err(protocol(
                        "the Claude CLI answered with a model other than the requested one",
                    ));
                }
                forward(event)
            }
            "content_block_start" => {
                let index = index.ok_or_else(|| protocol("a content block has no index"))?;
                let mut event = event.clone();
                let block = event
                    .get_mut("content_block")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| protocol("a content block has no body"))?;
                match block.get("type").and_then(Value::as_str) {
                    Some("text" | "thinking" | "redacted_thinking") => {}
                    Some("tool_use") => {
                        let cli_name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let restored = self.expect.aliases.restore(&cli_name).map(str::to_string);
                        if restored.is_none() && cli_name != TOLERATED_BUILTIN {
                            return Err(protocol(
                                "the Claude CLI called a tool Fontaine did not define",
                            ));
                        }
                        let id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                            .ok_or_else(|| protocol("a tool call has no id"))?
                            .to_string();
                        if self.calls.contains_key(&id) {
                            return Err(protocol("the Claude CLI reused a tool call id"));
                        }
                        self.calls.insert(
                            id,
                            Call {
                                cli_name,
                                denied: false,
                                echoed: false,
                            },
                        );
                        match restored {
                            Some(original) => {
                                block.insert("name".to_string(), Value::String(original));
                            }
                            None => {
                                self.suppressed.insert(index);
                                return Ok(Event::Nothing);
                            }
                        }
                    }
                    _ => {
                        return Err(protocol(
                            "the Claude CLI used a native tool or hosted content",
                        ));
                    }
                }
                self.open.insert(index);
                forward(&event)
            }
            "content_block_delta" => {
                let index = index.ok_or_else(|| protocol("a content delta has no index"))?;
                if self.suppressed.contains(&index) {
                    return Ok(Event::Nothing);
                }
                if !self.open.contains(&index) {
                    return Err(protocol(
                        "a content delta arrived for a block that is not open",
                    ));
                }
                forward(event)
            }
            "content_block_stop" => {
                let index = index.ok_or_else(|| protocol("a content stop has no index"))?;
                if self.suppressed.remove(&index) {
                    return Ok(Event::Nothing);
                }
                if !self.open.remove(&index) {
                    return Err(protocol("a content block stopped that was not open"));
                }
                forward(event)
            }
            "message_delta" => {
                let stop = event
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str);
                self.streamed_output_tokens = event
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_u64)
                    .or(self.streamed_output_tokens);
                match stop {
                    None => forward(event),
                    Some(reason) => {
                        self.stop_reason = Some(reason.to_string());
                        self.terminal = Some(frame_of(event));
                        Ok(Event::Nothing)
                    }
                }
            }
            "message_stop" => {
                if self.terminal.is_none() {
                    return Err(protocol("the response stopped without a stop reason"));
                }
                if !self.open.is_empty() {
                    return Err(Box::new(ProviderError::Truncated));
                }
                self.stopped = true;
                Ok(Event::Nothing)
            }
            "ping" => Ok(Event::Nothing),
            "error" => Err(self.native_error(None)),
            _ => Err(protocol(
                "the Claude CLI sent a stream event this adapter does not accept",
            )),
        }
    }

    /// The tool results a denied call leaves in the output: tolerated only as errors answering
    /// calls this response announced, once each. Anything else is native tool execution.
    fn denial_echo(&mut self, value: &Value) -> Result<Event, Failure> {
        let refused = || protocol("the Claude CLI executed a native tool or permission request");
        let blocks = value
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .filter(|blocks| !blocks.is_empty())
            .ok_or_else(refused)?;
        let mut ids = BTreeSet::new();
        for block in blocks {
            let answers = block.get("type").and_then(Value::as_str) == Some("tool_result")
                && block.get("is_error").and_then(Value::as_bool) == Some(true);
            let id = block.get("tool_use_id").and_then(Value::as_str);
            match id.filter(|_| answers) {
                Some(id)
                    if self.calls.get(id).is_some_and(|call| !call.echoed) && ids.insert(id) => {}
                _ => return Err(refused()),
            }
        }
        for id in ids {
            if let Some(call) = self.calls.get_mut(id) {
                call.echoed = true;
            }
        }
        Ok(Event::Nothing)
    }

    fn result(&mut self, value: &Value) -> Result<Event, Failure> {
        if self.result_seen {
            return Err(protocol("the Claude CLI sent a second result"));
        }
        let subtype = value
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let failed = value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if self.phase == Phase::Ack {
            let quiet = value
                .get("num_turns")
                .and_then(Value::as_u64)
                .is_none_or(|n| n == 0);
            if subtype == "success" && !failed && quiet {
                return Ok(Event::Ack);
            }
            return Err(if failed || subtype != "success" {
                self.native_error(Some(value))
            } else {
                protocol("the Claude CLI answered a history frame")
            });
        }
        self.result_seen = true;
        if !self.stopped {
            return Err(if failed || subtype != "success" {
                self.native_error(Some(value))
            } else if self.messages > 0 {
                Box::new(ProviderError::Truncated)
            } else {
                protocol("the Claude CLI reported success without a response")
            });
        }
        let tool_stop = self.stop_reason.as_deref() == Some("tool_use");
        let acceptable = match subtype {
            "success" => !failed,
            "error_max_turns" => tool_stop,
            _ => false,
        };
        if !acceptable {
            return Err(
                if failed || subtype != "success" && subtype != "error_max_turns" {
                    self.native_error(Some(value))
                } else {
                    protocol("the Claude CLI's result contradicts its response")
                },
            );
        }
        if !tool_stop && self.calls.values().any(|call| call.denied || call.echoed) {
            return Err(protocol("the Claude CLI's result contradicts its response"));
        }
        if let (Some(streamed), Some(reported)) = (
            self.streamed_output_tokens,
            value
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_u64),
        ) && reported < streamed
        {
            return Err(protocol("the Claude CLI's usage disagrees with its stream"));
        }
        Ok(Event::Complete)
    }

    /// The failure a native error result stands for, without echoing what the CLI said.
    fn native_error(&self, result: Option<&Value>) -> Failure {
        let status = result
            .and_then(|r| r.get("api_error_status"))
            .and_then(Value::as_u64);
        let by_status = match status {
            Some(401 | 403) => Some(Native::Auth),
            Some(402 | 429) => Some(Native::Limit),
            Some(500..=599) => Some(Native::Server),
            Some(400..=499) => Some(Native::Invalid),
            _ => None,
        };
        let by_text = result
            .and_then(|r| r.get("result"))
            .and_then(Value::as_str)
            .and_then(classify_text);
        match by_status.or(by_text).or_else(|| self.native.clone()) {
            Some(Native::Auth) => ClaudeError::new(
                ErrorKind::Auth,
                "the Claude CLI's login was refused; run `claude auth login` outside Fontaine",
            )
            .boxed(),
            Some(Native::Limit) => limit_error().boxed(),
            Some(Native::Server) => Box::new(ProviderError::from_http_response(
                http::StatusCode::from_u16(status.map_or(529, |s| s as u16))
                    .unwrap_or(http::StatusCode::from_u16(529).expect("529 is a valid status")),
                r#"{"type":"error","error":{"type":"api_error","message":"the Claude CLI reported a server error"}}"#,
            )),
            Some(Native::Invalid) | None => protocol("the Claude CLI reported an error"),
        }
    }
}

/// Failure categories read from text the CLI wrote, never shown.
pub(crate) fn classify_text(text: &str) -> Option<Native> {
    let text = text.to_lowercase();
    if [
        "not logged in",
        "/login",
        "invalid api key",
        "authentication",
    ]
    .iter()
    .any(|needle| text.contains(needle))
    {
        Some(Native::Auth)
    } else if [
        "usage limit",
        "rate limit",
        "limit reached",
        "credit",
        "billing",
    ]
    .iter()
    .any(|needle| text.contains(needle))
    {
        Some(Native::Limit)
    } else if text.contains("overloaded") {
        Some(Native::Server)
    } else {
        None
    }
}

/// The failure for a process that ended without a complete response, classified from its
/// stderr and exit status without showing either.
pub(crate) fn exit_failure(stderr: &str, native: Option<&Native>) -> Failure {
    match classify_text(stderr).or_else(|| native.cloned()) {
        Some(Native::Auth) => ClaudeError::new(
            ErrorKind::Auth,
            "the Claude CLI's login was refused; run `claude auth login` outside Fontaine",
        )
        .boxed(),
        Some(Native::Limit) => limit_error().boxed(),
        // A process that ended early may end on the next attempt, whether or not it began.
        _ => Box::new(ProviderError::Truncated),
    }
}

fn frame_of(event: &Value) -> WireFrame {
    WireFrame::Text(event.to_string())
}

fn forward(event: &Value) -> Result<Event, Failure> {
    Ok(Event::Frame(frame_of(event)))
}

#[cfg(test)]
mod tests;
