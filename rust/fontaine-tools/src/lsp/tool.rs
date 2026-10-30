//! The `lsp` tool: symbol navigation through a language server the user configured. It carries
//! locations and the server's own text only, never file contents Fontaine read, so a location
//! outside the workspace roots is a pointer the model must follow with the gated `read`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use lsp_types::request::{
    CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
    DocumentSymbolRequest, GotoDefinition, GotoImplementation, HoverRequest, References, Request,
    WorkspaceSymbolRequest,
};
use lsp_types::{
    CallHierarchyIncomingCallsParams, CallHierarchyItem, CallHierarchyOutgoingCallsParams,
    CallHierarchyPrepareParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
    GotoDefinitionParams, GotoDefinitionResponse, HoverContents, HoverParams, Location,
    MarkedString, OneOf, PartialResultParams, Position, ReferenceContext, ReferenceParams,
    SymbolInformation, SymbolKind, TextDocumentIdentifier, TextDocumentPositionParams, Url,
    WorkDoneProgressParams, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use serde::Deserialize;
use serde_json::json;

use super::client::{Encoding, RequestError, from_lsp_position, line_text, to_lsp_position};
use super::manager::{LspServers, LspWorkspace, Prepared};
use crate::scope::Scope;

/// Locations one answer names before it says how many more there are.
pub const MAX_LOCATIONS: usize = 100;
/// Characters of hover text an answer carries.
pub const MAX_HOVER_CHARS: usize = 4_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Operation {
    GoToDefinition,
    FindReferences,
    Hover,
    DocumentSymbol,
    WorkspaceSymbol,
    GoToImplementation,
    PrepareCallHierarchy,
    IncomingCalls,
    OutgoingCalls,
}

impl Operation {
    fn needs_position(self) -> bool {
        !matches!(self, Operation::DocumentSymbol | Operation::WorkspaceSymbol)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LspArgs {
    pub operation: Operation,
    pub path: String,
    pub line: Option<u32>,
    pub character: Option<u32>,
    pub query: Option<String>,
}

#[derive(Clone)]
pub struct LspTool {
    pub servers: Arc<LspServers>,
    pub workspace: Arc<dyn LspWorkspace>,
}

/// `path` relative to the primary root when it is inside it, absolute otherwise.
pub fn display_path(path: &Path, primary: &Path) -> String {
    path.strip_prefix(primary)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Turns the server's URIs and positions into the locations the model reads.
struct Locator {
    encoding: Encoding,
    scope: Arc<Scope>,
    texts: HashMap<Url, Option<String>>,
}

impl Locator {
    fn new(prepared: &Prepared, scope: Arc<Scope>) -> Locator {
        let mut texts = HashMap::new();
        texts.insert(prepared.uri.clone(), Some(prepared.text.clone()));
        Locator {
            encoding: prepared.client.encoding,
            scope,
            texts,
        }
    }

    /// `path:line:col`, 1-based, in characters as an editor shows them.
    fn place(&mut self, uri: &Url, position: Position) -> String {
        let path = uri.to_file_path().ok();
        let label = path.as_deref().map_or_else(
            || uri.to_string(),
            |path| display_path(path, self.scope.primary()),
        );
        // Columns need the line's text only when the server counts differently; Fontaine reads
        // a file for that only inside the roots.
        let text = if self.encoding == Encoding::Utf32 {
            None
        } else {
            let scope = &self.scope;
            self.texts
                .entry(uri.clone())
                .or_insert_with(|| {
                    let path = uri.to_file_path().ok()?;
                    scope.find(path.to_str()?).ok()?;
                    std::fs::read_to_string(path).ok()
                })
                .as_deref()
        };
        let (line, column) = from_lsp_position(
            self.encoding,
            text.and_then(|t| line_text(t, position.line)),
            position,
        );
        format!("{label}:{line}:{column}")
    }

    fn location(&mut self, location: &Location) -> String {
        self.place(&location.uri, location.range.start)
    }
}

fn kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::FILE => "file",
        SymbolKind::MODULE => "module",
        SymbolKind::NAMESPACE => "namespace",
        SymbolKind::PACKAGE => "package",
        SymbolKind::CLASS => "class",
        SymbolKind::METHOD => "method",
        SymbolKind::PROPERTY => "property",
        SymbolKind::FIELD => "field",
        SymbolKind::CONSTRUCTOR => "constructor",
        SymbolKind::ENUM => "enum",
        SymbolKind::INTERFACE => "interface",
        SymbolKind::FUNCTION => "function",
        SymbolKind::VARIABLE => "variable",
        SymbolKind::CONSTANT => "constant",
        SymbolKind::STRING => "string",
        SymbolKind::NUMBER => "number",
        SymbolKind::BOOLEAN => "boolean",
        SymbolKind::ARRAY => "array",
        SymbolKind::OBJECT => "object",
        SymbolKind::KEY => "key",
        SymbolKind::NULL => "null",
        SymbolKind::ENUM_MEMBER => "enum member",
        SymbolKind::STRUCT => "struct",
        SymbolKind::EVENT => "event",
        SymbolKind::OPERATOR => "operator",
        SymbolKind::TYPE_PARAMETER => "type parameter",
        _ => "symbol",
    }
}

/// `heading`, then at most [`MAX_LOCATIONS`] of `lines`, then how many were left out.
pub fn capped_list(heading: &str, lines: &[String]) -> String {
    capped_total(heading, lines, lines.len())
}

/// `lines` holds at most the first [`MAX_LOCATIONS`] of `total` entries.
fn capped_total(heading: &str, lines: &[String], total: usize) -> String {
    let mut out = heading.to_string();
    for line in lines.iter().take(MAX_LOCATIONS) {
        out.push('\n');
        out.push_str(line);
    }
    if total > MAX_LOCATIONS {
        out.push_str(&format!("\n… and {} more", total - MAX_LOCATIONS));
    }
    out
}

pub fn capped_hover(text: &str) -> String {
    match text.char_indices().nth(MAX_HOVER_CHARS) {
        Some((end, _)) => format!("{}\n… (truncated)", &text[..end]),
        None => text.to_string(),
    }
}

fn plural(count: usize, one: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {one}s")
    }
}

fn locations_of(response: GotoDefinitionResponse) -> Vec<Location> {
    match response {
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .map(|link| Location {
                uri: link.target_uri,
                range: link.target_selection_range,
            })
            .collect(),
    }
}

fn marked_text(marked: MarkedString) -> String {
    match marked {
        MarkedString::String(text) => text,
        MarkedString::LanguageString(language) => language.value,
    }
}

fn hover_text(contents: HoverContents) -> String {
    match contents {
        HoverContents::Scalar(marked) => marked_text(marked),
        HoverContents::Array(items) => items
            .into_iter()
            .map(marked_text)
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(markup) => markup.value,
    }
}

fn nested_symbols(
    locator: &mut Locator,
    uri: &Url,
    symbols: &[DocumentSymbol],
    depth: usize,
    out: &mut Vec<String>,
) {
    for symbol in symbols {
        out.push(format!(
            "{}{} ({}) {}",
            "  ".repeat(depth),
            symbol.name,
            kind_name(symbol.kind),
            locator.place(uri, symbol.selection_range.start)
        ));
        if let Some(children) = &symbol.children {
            nested_symbols(locator, uri, children, depth + 1, out);
        }
    }
}

fn flat_symbol(locator: &mut Locator, symbol: &SymbolInformation) -> String {
    let container = symbol
        .container_name
        .as_deref()
        .filter(|c| !c.is_empty())
        .map_or_else(String::new, |c| format!(" in {c}"));
    format!(
        "{} ({}){container} {}",
        symbol.name,
        kind_name(symbol.kind),
        locator.location(&symbol.location)
    )
}

fn item_line(locator: &mut Locator, item: &CallHierarchyItem) -> String {
    format!(
        "{} ({}) {}",
        item.name,
        kind_name(item.kind),
        locator.place(&item.uri, item.selection_range.start)
    )
}

impl LspTool {
    fn position(
        &self,
        prepared: &Prepared,
        args: &LspArgs,
    ) -> Result<Position, ToolExecutionError> {
        let need = |value: Option<u32>, name: &str| {
            value.ok_or_else(|| {
                ToolExecutionError::invalid_args(format!(
                    "`{name}` is required for this operation (1-based)"
                ))
            })
        };
        let line = need(args.line, "line")?;
        let character = need(args.character, "character")?;
        to_lsp_position(prepared.client.encoding, &prepared.text, line, character)
            .map_err(|e| ToolExecutionError::invalid_args(e.to_string()))
    }

    async fn ask<R: Request>(
        &self,
        prepared: &Prepared,
        params: R::Params,
    ) -> Result<R::Result, ToolExecutionError> {
        let server = &prepared.server;
        match prepared.client.request::<R>(params).await {
            Ok(result) => Ok(result),
            Err(RequestError::Timeout(after)) => Err(ToolExecutionError::timeout(format!(
                "{server} did not answer within {} seconds",
                after.as_secs_f64()
            ))),
            Err(RequestError::Stopped) => Err(ToolExecutionError::other(format!(
                "{server} crashed ({}); the next call may restart it",
                prepared.client.exit_detail().await
            ))),
            Err(RequestError::Server(message)) => Err(ToolExecutionError::other(format!(
                "{server} returned an error: {message}"
            ))),
        }
    }

    fn empty(prepared: &Prepared) -> ToolExecutionError {
        ToolExecutionError::other(format!(
            "{} returned no result (it may still be indexing)",
            prepared.server
        ))
    }

    fn at(prepared: &Prepared, position: Position) -> TextDocumentPositionParams {
        TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: prepared.uri.clone(),
            },
            position,
        }
    }

    async fn locations(
        &self,
        prepared: &Prepared,
        locator: &mut Locator,
        args: &LspArgs,
    ) -> Result<String, ToolExecutionError> {
        let position = self.position(prepared, args)?;
        let params = GotoDefinitionParams {
            text_document_position_params: Self::at(prepared, position),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let (found, noun) = match args.operation {
            Operation::GoToDefinition => (
                self.ask::<GotoDefinition>(prepared, params)
                    .await?
                    .map(locations_of),
                "definition",
            ),
            Operation::GoToImplementation => (
                self.ask::<GotoImplementation>(prepared, params)
                    .await?
                    .map(locations_of),
                "implementation",
            ),
            _ => (
                self.ask::<References>(
                    prepared,
                    ReferenceParams {
                        text_document_position: Self::at(prepared, position),
                        work_done_progress_params: WorkDoneProgressParams::default(),
                        partial_result_params: PartialResultParams::default(),
                        context: ReferenceContext {
                            include_declaration: true,
                        },
                    },
                )
                .await?,
                "reference",
            ),
        };
        let found = found
            .filter(|f| !f.is_empty())
            .ok_or_else(|| Self::empty(prepared))?;
        let lines: Vec<String> = found
            .iter()
            .take(MAX_LOCATIONS)
            .map(|l| locator.location(l))
            .collect();
        Ok(capped_total(
            &format!("{}:", plural(found.len(), noun)),
            &lines,
            found.len(),
        ))
    }

    async fn symbols(
        &self,
        prepared: &Prepared,
        locator: &mut Locator,
    ) -> Result<String, ToolExecutionError> {
        let response = self
            .ask::<DocumentSymbolRequest>(
                prepared,
                DocumentSymbolParams {
                    text_document: TextDocumentIdentifier {
                        uri: prepared.uri.clone(),
                    },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                },
            )
            .await?;
        let mut lines = Vec::new();
        match response {
            Some(DocumentSymbolResponse::Nested(symbols)) => {
                nested_symbols(locator, &prepared.uri, &symbols, 0, &mut lines);
            }
            Some(DocumentSymbolResponse::Flat(symbols)) => {
                lines.extend(symbols.iter().map(|s| flat_symbol(locator, s)));
            }
            None => {}
        }
        if lines.is_empty() {
            return Err(Self::empty(prepared));
        }
        Ok(capped_list(
            &format!("{}:", plural(lines.len(), "symbol")),
            &lines,
        ))
    }

    async fn workspace_symbols(
        &self,
        prepared: &Prepared,
        locator: &mut Locator,
        query: &str,
    ) -> Result<String, ToolExecutionError> {
        let response = self
            .ask::<WorkspaceSymbolRequest>(
                prepared,
                WorkspaceSymbolParams {
                    query: query.to_string(),
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                },
            )
            .await?;
        let (total, lines): (usize, Vec<String>) = match response {
            Some(WorkspaceSymbolResponse::Flat(symbols)) => (
                symbols.len(),
                symbols
                    .iter()
                    .take(MAX_LOCATIONS)
                    .map(|s| flat_symbol(locator, s))
                    .collect(),
            ),
            Some(WorkspaceSymbolResponse::Nested(symbols)) => (
                symbols.len(),
                symbols
                    .iter()
                    .take(MAX_LOCATIONS)
                    .map(|symbol| {
                        let place = match &symbol.location {
                            OneOf::Left(location) => locator.location(location),
                            // A server may leave the range for a later resolve; the file is
                            // still a pointer.
                            OneOf::Right(file) => locator.place(&file.uri, Position::new(0, 0)),
                        };
                        format!("{} ({}) {place}", symbol.name, kind_name(symbol.kind))
                    })
                    .collect(),
            ),
            None => (0, Vec::new()),
        };
        if lines.is_empty() {
            return Err(Self::empty(prepared));
        }
        Ok(capped_total(
            &format!("{}:", plural(total, "symbol")),
            &lines,
            total,
        ))
    }

    async fn hover(
        &self,
        prepared: &Prepared,
        args: &LspArgs,
    ) -> Result<String, ToolExecutionError> {
        let position = self.position(prepared, args)?;
        let hover = self
            .ask::<HoverRequest>(
                prepared,
                HoverParams {
                    text_document_position_params: Self::at(prepared, position),
                    work_done_progress_params: WorkDoneProgressParams::default(),
                },
            )
            .await?;
        let text = hover.map(|h| hover_text(h.contents)).unwrap_or_default();
        if text.trim().is_empty() {
            return Err(Self::empty(prepared));
        }
        Ok(capped_hover(text.trim()))
    }

    async fn call_hierarchy(
        &self,
        prepared: &Prepared,
        locator: &mut Locator,
        args: &LspArgs,
    ) -> Result<String, ToolExecutionError> {
        let position = self.position(prepared, args)?;
        let items = self
            .ask::<CallHierarchyPrepare>(
                prepared,
                CallHierarchyPrepareParams {
                    text_document_position_params: Self::at(prepared, position),
                    work_done_progress_params: WorkDoneProgressParams::default(),
                },
            )
            .await?
            .filter(|items| !items.is_empty())
            .ok_or_else(|| Self::empty(prepared))?;
        if args.operation == Operation::PrepareCallHierarchy {
            let lines: Vec<String> = items.iter().map(|i| item_line(locator, i)).collect();
            return Ok(capped_list(
                &format!("{}:", plural(lines.len(), "item")),
                &lines,
            ));
        }
        let incoming = args.operation == Operation::IncomingCalls;
        let mut lines = Vec::new();
        for item in &items {
            if incoming {
                let calls = self
                    .ask::<CallHierarchyIncomingCalls>(
                        prepared,
                        CallHierarchyIncomingCallsParams {
                            item: item.clone(),
                            work_done_progress_params: WorkDoneProgressParams::default(),
                            partial_result_params: PartialResultParams::default(),
                        },
                    )
                    .await?
                    .unwrap_or_default();
                for call in calls {
                    lines.push(format!(
                        "{} [{}]",
                        item_line(locator, &call.from),
                        plural(call.from_ranges.len(), "call site")
                    ));
                }
            } else {
                let calls = self
                    .ask::<CallHierarchyOutgoingCalls>(
                        prepared,
                        CallHierarchyOutgoingCallsParams {
                            item: item.clone(),
                            work_done_progress_params: WorkDoneProgressParams::default(),
                            partial_result_params: PartialResultParams::default(),
                        },
                    )
                    .await?
                    .unwrap_or_default();
                for call in calls {
                    lines.push(format!(
                        "{} [{}]",
                        item_line(locator, &call.to),
                        plural(call.from_ranges.len(), "call site")
                    ));
                }
            }
        }
        if lines.is_empty() {
            return Err(Self::empty(prepared));
        }
        let heading = if incoming {
            format!(
                "{} of {}:",
                plural(lines.len(), "incoming call"),
                items[0].name
            )
        } else {
            format!(
                "{} from {}:",
                plural(lines.len(), "outgoing call"),
                items[0].name
            )
        };
        Ok(capped_list(&heading, &lines))
    }
}

impl Tool for LspTool {
    const NAME: &'static str = "lsp";
    type Args = LspArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Code intelligence through the project's language server: find where a symbol is \
         defined or implemented, list its references, show hover information, outline a file's \
         symbols, search symbols across the workspace, or walk the call hierarchy. Prefer it \
         to grep for these questions when it covers the file's language. `line` and \
         `character` are 1-based, as an editor shows them, and point at the symbol; they are \
         not used for documentSymbol and workspaceSymbol. workspaceSymbol needs a `query`. \
         Results are locations as path:line:col; read the file to see the code."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": [
                        "goToDefinition", "findReferences", "hover", "documentSymbol",
                        "workspaceSymbol", "goToImplementation", "prepareCallHierarchy",
                        "incomingCalls", "outgoingCalls"
                    ],
                    "description": "What to ask the language server.",
                },
                "path": {
                    "type": "string",
                    "description": "The file the question is about; its extension picks the language server.",
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based line of the symbol.",
                },
                "character": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based character of the symbol within the line.",
                },
                "query": {
                    "type": "string",
                    "description": "For workspaceSymbol: the name to search for.",
                },
            },
            "required": ["operation", "path"],
            "additionalProperties": false,
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: LspArgs,
    ) -> Result<String, ToolExecutionError> {
        let scope = self.workspace.scope();
        // The gate cannot refuse an out-of-root path here: an allow rule or a hook allow
        // returns before its containment check, and this tool is not routed per root.
        let (_, path) = scope.find(&args.path).map_err(|_| {
            ToolExecutionError::other(format!(
                "{} is outside the workspace roots; the language server is asked only about files inside them",
                args.path
            ))
        })?;
        let query = args.query.as_deref().map(str::trim).unwrap_or_default();
        if args.operation == Operation::WorkspaceSymbol && query.is_empty() {
            return Err(ToolExecutionError::invalid_args(
                "`query` is required for workspaceSymbol",
            ));
        }
        if args.operation.needs_position() && (args.line.is_none() || args.character.is_none()) {
            return Err(ToolExecutionError::invalid_args(
                "`line` and `character` are required for this operation (1-based)",
            ));
        }
        let prepared = self.servers.prepare(&path).await?;
        let mut locator = Locator::new(&prepared, scope);
        match args.operation {
            Operation::GoToDefinition
            | Operation::GoToImplementation
            | Operation::FindReferences => self.locations(&prepared, &mut locator, &args).await,
            Operation::Hover => self.hover(&prepared, &args).await,
            Operation::DocumentSymbol => self.symbols(&prepared, &mut locator).await,
            Operation::WorkspaceSymbol => {
                self.workspace_symbols(&prepared, &mut locator, query).await
            }
            Operation::PrepareCallHierarchy
            | Operation::IncomingCalls
            | Operation::OutgoingCalls => self.call_hierarchy(&prepared, &mut locator, &args).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_inside_the_primary_root_are_relative_and_others_absolute() {
        let primary = Path::new("/work/app");
        assert_eq!(
            display_path(Path::new("/work/app/src/main.rs"), primary),
            "src/main.rs"
        );
        assert_eq!(
            display_path(Path::new("/home/me/.cargo/registry/lib.rs"), primary),
            "/home/me/.cargo/registry/lib.rs"
        );
    }

    #[test]
    fn a_list_names_a_hundred_locations_then_counts_the_rest() {
        let lines: Vec<String> = (0..130).map(|n| format!("a.rs:{n}:1")).collect();
        let text = capped_list("130 references:", &lines);
        assert_eq!(text.lines().count(), 1 + 100 + 1);
        assert!(text.contains("a.rs:99:1"));
        assert!(!text.contains("a.rs:100:1"));
        assert!(text.ends_with("… and 30 more"));
        let short = capped_list("2 references:", &lines[..2]);
        assert_eq!(short, "2 references:\na.rs:0:1\na.rs:1:1");
    }

    #[test]
    fn hover_text_is_cut_at_four_thousand_characters() {
        let long = "é".repeat(4_500);
        let cut = capped_hover(&long);
        assert!(cut.starts_with(&"é".repeat(4_000)));
        assert!(cut.ends_with("… (truncated)"));
        assert_eq!(capped_hover("short"), "short");
    }

    #[test]
    fn symbol_kinds_read_as_words() {
        assert_eq!(kind_name(SymbolKind::FUNCTION), "function");
        assert_eq!(kind_name(SymbolKind::STRUCT), "struct");
        assert_eq!(kind_name(SymbolKind::ENUM_MEMBER), "enum member");
    }

    #[test]
    fn the_operations_are_claude_codes_names() {
        let args: LspArgs = serde_json::from_value(json!({
            "operation": "prepareCallHierarchy", "path": "a.rs", "line": 1, "character": 2
        }))
        .unwrap();
        assert_eq!(args.operation, Operation::PrepareCallHierarchy);
        for name in [
            "goToDefinition",
            "findReferences",
            "hover",
            "documentSymbol",
            "workspaceSymbol",
            "goToImplementation",
            "prepareCallHierarchy",
            "incomingCalls",
            "outgoingCalls",
        ] {
            serde_json::from_value::<LspArgs>(json!({"operation": name, "path": "a.rs"})).unwrap();
        }
    }
}
