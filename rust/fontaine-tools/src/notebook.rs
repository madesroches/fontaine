//! Jupyter notebook support: the cell-level `notebook_edit` tool and a `read` that shows a
//! notebook as cells instead of raw JSON. The notebook model is pure functions over a parsed
//! `serde_json::Value` so key order and fields we do not know survive an edit.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rig_core::completion::ToolAnnotations;
use rig_core::message::{ImageMediaType, ToolResultContent};
use rig_core::tool::{
    FileRead, FilesChanged, FilesRead, Scope, Tool, ToolContext, ToolExecutionError, ToolOutput,
    Touches, content_hash,
};
use rig_tools::fs::{file_change, io_error};
use rig_tools::workspace::{Local, Workspace};
use rig_tools::{Read, ReadArgs, Seen, blocking};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::gate::InputModalities;

const NOTEBOOK_EXTENSION: &str = "ipynb";

/// The most bytes of image a `read` sends the model: providers refuse bigger ones
/// (Anthropic takes 5 MB per image), the cap rig's own attach uses.
const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CellType {
    Code,
    Markdown,
}

impl CellType {
    fn as_str(self) -> &'static str {
        match self {
            CellType::Code => "code",
            CellType::Markdown => "markdown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditMode {
    #[default]
    Replace,
    Insert,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotebookEditArgs {
    pub path: String,
    pub cell_id: Option<String>,
    pub cell_type: Option<CellType>,
    #[serde(default)]
    pub edit_mode: EditMode,
    pub new_source: Option<String>,
}

pub fn is_notebook_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(NOTEBOOK_EXTENSION))
}

/// A parsed notebook with the formatting needed to write it back as it was found.
#[derive(Clone, Debug, PartialEq)]
pub struct Notebook {
    root: Value,
    indent: usize,
    trailing_newline: bool,
}

/// What an edit did to one cell, for the reply and the approval diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edited {
    pub summary: String,
    /// The cell's source before the edit; none for an inserted cell.
    pub before: Option<String>,
    /// The cell's source after the edit; none for a deleted cell.
    pub after: Option<String>,
}

impl Notebook {
    pub fn parse(text: &str) -> Result<Notebook, String> {
        let root: Value = serde_json::from_str(text)
            .map_err(|e| format!("the file is not valid notebook JSON: {e}"))?;
        let Some(object) = root.as_object() else {
            return Err("the file is not a notebook: its top level is not an object".into());
        };
        if !object.get("cells").is_some_and(Value::is_array) {
            return Err("the file is not a notebook: it has no `cells` array".into());
        }
        match object.get("nbformat").and_then(Value::as_u64) {
            Some(version) if version >= 4 => {}
            Some(version) => {
                return Err(format!(
                    "nbformat {version} is not supported; only nbformat 4 and later notebooks \
                     can be edited"
                ));
            }
            None => return Err("the file is not a notebook: it has no `nbformat`".into()),
        }
        Ok(Notebook {
            root,
            indent: detect_indent(text),
            trailing_newline: text.ends_with('\n'),
        })
    }

    fn cells(&self) -> &Vec<Value> {
        self.root["cells"]
            .as_array()
            .expect("parse checked that `cells` is an array")
    }

    fn cells_mut(&mut self) -> &mut Vec<Value> {
        self.root["cells"]
            .as_array_mut()
            .expect("parse checked that `cells` is an array")
    }

    /// Cells carry an `id` from nbformat 4.5 on.
    fn has_cell_ids(&self) -> bool {
        self.root.get("nbformat_minor").and_then(Value::as_u64) >= Some(5)
    }

    /// The cell named by `id`, or by `cell-N` (0-based index) when no cell has that id.
    fn find(&self, id: &str) -> Result<usize, String> {
        let cells = self.cells();
        let by_id = cells
            .iter()
            .position(|cell| cell.get("id").and_then(Value::as_str) == Some(id));
        let by_index = id
            .strip_prefix("cell-")
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n < cells.len());
        by_id.or(by_index).ok_or_else(|| {
            format!(
                "no cell has the id `{id}`; the notebook has {} cells, addressable by id or as \
                 `cell-0` to `cell-{}`",
                cells.len(),
                cells.len().saturating_sub(1)
            )
        })
    }

    fn fresh_id(&self) -> String {
        loop {
            let candidate: String = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
            let taken = self
                .cells()
                .iter()
                .any(|cell| cell.get("id").and_then(Value::as_str) == Some(candidate.as_str()));
            if !taken {
                return candidate;
            }
        }
    }

    pub fn apply(&mut self, args: &NotebookEditArgs) -> Result<Edited, String> {
        match args.edit_mode {
            EditMode::Replace => self.replace(args),
            EditMode::Insert => self.insert(args),
            EditMode::Delete => self.delete(args),
        }
    }

    fn replace(&mut self, args: &NotebookEditArgs) -> Result<Edited, String> {
        let id = args
            .cell_id
            .as_deref()
            .ok_or("`cell_id` is required to replace a cell")?;
        let source = args
            .new_source
            .as_deref()
            .ok_or("`new_source` is required to replace a cell")?;
        let index = self.find(id)?;
        let cell = self.cells_mut()[index]
            .as_object_mut()
            .ok_or_else(|| format!("cell `{id}` is not an object"))?;
        let before = source_of(cell);
        let current = cell
            .get("cell_type")
            .and_then(Value::as_str)
            .unwrap_or("code")
            .to_string();
        let retyped = args.cell_type.filter(|t| t.as_str() != current);
        match retyped {
            Some(new_type) => {
                cell.insert("cell_type".into(), json!(new_type.as_str()));
                cell.insert("source".into(), lines_of(source));
            }
            None => {
                let keep_string = cell.get("source").is_some_and(Value::is_string);
                let value = if keep_string {
                    Value::String(source.to_string())
                } else {
                    lines_of(source)
                };
                cell.insert("source".into(), value);
            }
        }
        let now = retyped.map_or(current.as_str(), |t| t.as_str());
        match now {
            "code" => {
                cell.insert("outputs".into(), json!([]));
                cell.insert("execution_count".into(), Value::Null);
            }
            _ => {
                cell.remove("outputs");
                cell.remove("execution_count");
            }
        }
        Ok(Edited {
            summary: format!("Replaced the source of cell {}", describe(index, cell)),
            before: Some(before),
            after: Some(source.to_string()),
        })
    }

    fn insert(&mut self, args: &NotebookEditArgs) -> Result<Edited, String> {
        let source = args
            .new_source
            .as_deref()
            .ok_or("`new_source` is required to insert a cell")?;
        let cell_type = args
            .cell_type
            .ok_or("`cell_type` is required to insert a cell")?;
        let at = match args.cell_id.as_deref() {
            Some(id) => self.find(id)? + 1,
            None => 0,
        };
        let mut cell = Map::new();
        cell.insert("cell_type".into(), json!(cell_type.as_str()));
        if cell_type == CellType::Code {
            cell.insert("execution_count".into(), Value::Null);
        }
        if self.has_cell_ids() {
            cell.insert("id".into(), Value::String(self.fresh_id()));
        }
        cell.insert("metadata".into(), json!({}));
        if cell_type == CellType::Code {
            cell.insert("outputs".into(), json!([]));
        }
        cell.insert("source".into(), lines_of(source));
        let summary = format!("Inserted a new cell at {}", describe(at, &cell));
        self.cells_mut().insert(at, Value::Object(cell));
        Ok(Edited {
            summary,
            before: None,
            after: Some(source.to_string()),
        })
    }

    fn delete(&mut self, args: &NotebookEditArgs) -> Result<Edited, String> {
        let id = args
            .cell_id
            .as_deref()
            .ok_or("`cell_id` is required to delete a cell")?;
        let index = self.find(id)?;
        let removed = self.cells_mut().remove(index);
        let object = removed.as_object().cloned().unwrap_or_default();
        Ok(Edited {
            summary: format!("Deleted cell {}", describe(index, &object)),
            before: Some(source_of(&object)),
            after: None,
        })
    }

    pub fn serialize(&self) -> String {
        let indent = vec![b' '; self.indent];
        let formatter = serde_json::ser::PrettyFormatter::with_indent(&indent);
        let mut out = Vec::new();
        let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
        serde::Serialize::serialize(&self.root, &mut serializer)
            .expect("a parsed JSON value serializes to memory");
        let mut text = String::from_utf8(out).expect("serde_json writes UTF-8");
        if self.trailing_newline {
            text.push('\n');
        }
        text
    }

    /// The notebook as cells, with their ids, sources and text outputs.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (index, cell) in self.cells().iter().enumerate() {
            let kind = cell.get("cell_type").and_then(Value::as_str).unwrap_or("?");
            let _ = write!(out, "[cell {index}");
            if let Some(id) = cell.get("id").and_then(Value::as_str) {
                let _ = write!(out, " id={id}");
            }
            let _ = writeln!(out, " type={kind}]");
            if let Some(object) = cell.as_object() {
                push_block(&mut out, &source_of(object));
            }
            for output in cell
                .get("outputs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                render_output(&mut out, output);
            }
            out.push('\n');
        }
        out
    }
}

/// Nbformat's own indent is 1; an indented notebook keeps whatever it used.
fn detect_indent(text: &str) -> usize {
    text.lines()
        .skip(1)
        .find(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start_matches(' ').len())
        .filter(|n| *n > 0)
        .unwrap_or(1)
}

fn describe(index: usize, cell: &Map<String, Value>) -> String {
    match cell.get("id").and_then(Value::as_str) {
        Some(id) => format!("{index} (id `{id}`)"),
        None => format!("{index} (`cell-{index}`)"),
    }
}

fn source_of(cell: &Map<String, Value>) -> String {
    match cell.get("source") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

/// Each line keeps its newline; the last has none unless the text ends with one.
fn lines_of(text: &str) -> Value {
    Value::Array(
        text.split_inclusive('\n')
            .map(|line| Value::String(line.to_string()))
            .collect(),
    )
}

fn push_block(out: &mut String, text: &str) {
    out.push_str(text);
    if !text.is_empty() && !text.ends_with('\n') {
        out.push('\n');
    }
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(lines) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

fn render_output(out: &mut String, output: &Value) {
    match output.get("output_type").and_then(Value::as_str) {
        Some("stream") => {
            let name = output
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("stdout");
            let _ = writeln!(out, "[output: {name}]");
            push_block(out, &text_of(&output["text"]));
        }
        Some("error") => {
            let name = output
                .get("ename")
                .and_then(Value::as_str)
                .unwrap_or("error");
            let value = output.get("evalue").and_then(Value::as_str).unwrap_or("");
            let _ = writeln!(out, "[output: error {name}: {value}]");
            let trace: Vec<&str> = output
                .get("traceback")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            push_block(out, &strip_ansi(&trace.join("\n")));
        }
        Some(kind @ ("execute_result" | "display_data")) => {
            let _ = writeln!(out, "[output: {kind}]");
            let Some(data) = output.get("data").and_then(Value::as_object) else {
                return;
            };
            if let Some(plain) = data.get("text/plain") {
                push_block(out, &text_of(plain));
            }
            for mime in data.keys().filter(|mime| mime.starts_with("image/")) {
                let _ = writeln!(out, "[{mime} output omitted]");
            }
        }
        _ => {}
    }
}

/// Tracebacks carry terminal colour codes the model has no use for.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The cell source an edit would remove and add, for the approval diff.
pub fn cell_change(text: &str, args: &Value) -> Option<Edited> {
    let args: NotebookEditArgs = serde_json::from_value(args.clone()).ok()?;
    let mut notebook = Notebook::parse(text).ok()?;
    notebook.apply(&args).ok()
}

fn invalid(message: impl Into<String>) -> ToolExecutionError {
    ToolExecutionError::invalid_args(message)
}

/// Edits one cell of a notebook, from the text `read` showed of it.
#[derive(Clone, Debug)]
pub struct NotebookEdit {
    pub workspace: Arc<dyn Workspace>,
    pub base: Option<PathBuf>,
    pub confine: bool,
    pub seen: Seen,
}

impl Default for NotebookEdit {
    fn default() -> Self {
        NotebookEdit {
            workspace: Arc::new(Local),
            base: None,
            confine: false,
            seen: Seen::default(),
        }
    }
}

impl Tool for NotebookEdit {
    const NAME: &'static str = "notebook_edit";
    type Args = NotebookEditArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Edit one cell of a Jupyter notebook (.ipynb): `replace` a cell's source (default), \
         `insert` a new cell after `cell_id` (at the start when none), or `delete` a cell. Read \
         the notebook with `read` first; it shows each cell with its id. Name a cell by its id, \
         or `cell-N` (0-based) when it has none. Replacing a code cell clears its outputs. Use \
         this, not `edit` or `write`, on notebooks."
            .to_owned()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The notebook to edit." },
                "cell_id": {
                    "type": "string",
                    "description": "The cell to replace or delete, or to insert after (none: insert at the start). An id, or `cell-N`."
                },
                "cell_type": {
                    "type": "string",
                    "enum": ["code", "markdown"],
                    "description": "The type of the cell; required to insert, and changes the type when replacing."
                },
                "edit_mode": {
                    "type": "string",
                    "enum": ["replace", "insert", "delete"],
                    "description": "What to do; default replace."
                },
                "new_source": {
                    "type": "string",
                    "description": "The cell's new source; required except to delete."
                },
            },
            "required": ["path"],
            "additionalProperties": false,
        })
    }

    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations::default()
            .with_destructive(true)
            .with_idempotent(false)
            .with_open_world(false)
    }

    fn footprint(&self, args: &NotebookEditArgs) -> Touches {
        Touches::Writes(Scope::Paths(vec![
            self.workspace.resolve(self.base.as_deref(), &args.path),
        ]))
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: NotebookEditArgs,
    ) -> Result<String, ToolExecutionError> {
        if !is_notebook_path(&args.path) {
            return Err(invalid(format!(
                "{} is not a notebook; `notebook_edit` only edits .ipynb files",
                args.path
            )));
        }
        let path = self
            .workspace
            .place(self.base.as_deref(), self.confine, &args.path)?;
        let workspace = self.workspace.clone();
        let seen = self.seen.clone();
        let (reply, change) = blocking(move || edit(&*workspace, &seen, path, &args)).await?;
        context.insert_result(FilesChanged(vec![change]))?;
        Ok(reply)
    }
}

fn edit(
    workspace: &dyn Workspace,
    seen: &Seen,
    resolved: PathBuf,
    args: &NotebookEditArgs,
) -> Result<(String, rig_core::tool::FileChange), ToolExecutionError> {
    let shown = resolved.to_string_lossy().into_owned();
    let Some((hash, text)) = seen.text(&resolved) else {
        return Err(invalid(format!(
            "{} has not been read in whole; read it with `read` before editing it (a notebook \
             over 8 MiB cannot be edited)",
            args.path
        )));
    };
    let mut notebook =
        Notebook::parse(&text).map_err(|e| invalid(format!("{}: {e}", args.path)))?;
    let edited = notebook.apply(args).map_err(invalid)?;
    let written = notebook.serialize();
    match workspace.write_if(&resolved, written.as_bytes(), &hash) {
        Ok(None) => {}
        Ok(Some(_)) => {
            return Err(invalid(format!(
                "{} changed since it was read, so nothing was written; read it again",
                args.path
            )));
        }
        Err(error) => {
            seen.forget(&resolved);
            return Err(io_error(&shown, error));
        }
    }
    seen.saw_whole(&resolved, &written);
    let change = file_change(&shown, Some(&text), Some(&written));
    Ok((format!("{} in {}.", edited.summary, args.path), change))
}

/// `read`, presenting more than notebooks: a Jupyter notebook (.ipynb) is shown as its cells,
/// an image goes to the model as an image content part when the calling agent's model accepts
/// images, and anything else reads as rig's `Read` does.
#[derive(Clone, Debug)]
pub struct ReadView {
    pub read: Read,
}

/// What the first bytes of a file read as.
enum Sniffed {
    Image(ImageMediaType),
    Pdf,
    Other,
}

fn sniff(bytes: &[u8]) -> Sniffed {
    if let Some(media) = ImageMediaType::sniff(bytes) {
        return Sniffed::Image(media);
    }
    if bytes.starts_with(b"%PDF-") {
        return Sniffed::Pdf;
    }
    Sniffed::Other
}

/// `size` the way the result header names it: `231 KB`, `1.2 MB`.
pub fn human_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    if size >= MB {
        format!("{:.1} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{} KB", size.div_ceil(KB))
    } else {
        format!("{size} B")
    }
}

impl ReadView {
    /// The model's accepted modalities for this call; absent means text-only, the safe
    /// fallback for callers that never insert them.
    fn modalities(context: &ToolContext) -> rig_core::catalog::Modalities {
        context
            .get::<InputModalities>()
            .ok()
            .flatten()
            .map_or_else(rig_core::catalog::Modalities::default, |m| m.0)
    }

    /// Reads the resolved `path` as an image: a text header naming the file, kind and size,
    /// then the image itself. Over the size cap, or for a model that takes no images, the
    /// result is a text explanation instead.
    async fn image(
        &self,
        context: &mut ToolContext,
        path: &Path,
        media: ImageMediaType,
        size: u64,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let kind = format!("{:?}", media).to_uppercase();
        let shown = path.to_string_lossy().into_owned();
        if size > MAX_IMAGE_BYTES {
            return Ok(ToolOutput::text(format!(
                "{shown} is an image ({kind}, {}), over the {} MB limit `read` sends; no part of \
                 it was shown. Read it in smaller pieces another way.",
                human_size(size),
                MAX_IMAGE_BYTES / (1024 * 1024),
            )));
        }
        let workspace = self.read.workspace.clone();
        let seen = self.read.seen.clone();
        let probe = path.to_path_buf();
        let shown_for_read = shown.clone();
        let bytes = blocking(move || {
            let contents = workspace
                .read(&probe, MAX_IMAGE_BYTES)
                .map_err(|e| io_error(&shown_for_read, e))?;
            Ok(contents.bytes)
        })
        .await?;
        let hash = content_hash(&bytes);
        seen.saw(path, hash.clone());
        context.insert_result(FilesRead(vec![FileRead::new(&shown, hash)]))?;
        if !Self::modalities(context).image {
            return Ok(ToolOutput::text(format!(
                "{shown} is an image ({kind}, {}). The current model does not accept images, so \
                 it cannot be shown; ask for it as an attachment or switch to a model that takes \
                 images.",
                human_size(size),
            )));
        }
        let header = format!("[image: {kind} · {}] {shown}", human_size(size));
        let image = ToolResultContent::Image(rig_core::message::Image {
            data: rig_core::message::DocumentSourceKind::base64(STANDARD.encode(&bytes)),
            media_type: Some(media),
            detail: None,
            native: None,
        });
        let output = ToolOutput::content(vec![ToolResultContent::text(header), image])
            .expect("an image result has a text header part");
        Ok(output)
    }

    /// A PDF never leaves `read` as a tool result: the wire rejects document blocks there, so
    /// the result is a stub naming the file and pointing at attaching it to the prompt.
    fn pdf(&self, path: &Path, size: u64) -> ToolOutput {
        ToolOutput::text(format!(
            "{} is a PDF ({}). PDFs are not sent as `read` results; attach the file to your \
             message (drop it on the prompt) so it reaches the model as a document.",
            path.display(),
            human_size(size),
        ))
    }

    /// A notebook reads as its cells; one that is not an editable notebook reads as plain
    /// text through rig's `Read`.
    async fn notebook(
        &self,
        context: &mut ToolContext,
        args: ReadArgs,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let path =
            self.read
                .workspace
                .place(self.read.base.as_deref(), self.read.confine, &args.path)?;
        let workspace = self.read.workspace.clone();
        let seen = self.read.seen.clone();
        let parsed = blocking(move || {
            let raw = workspace.read_text(&path)?;
            let Ok(notebook) = Notebook::parse(&raw) else {
                return Ok::<_, ToolExecutionError>(Err(args));
            };
            // The raw text is what an edit starts from, not the rendering.
            seen.saw_whole(&path, &raw);
            let (output, lines) = args.shown(&notebook.render());
            let read = FileRead::new(path.to_string_lossy(), content_hash(raw.as_bytes()))
                .with_lines(lines);
            Ok(Ok((output, read)))
        })
        .await?;
        // Not an editable notebook (invalid JSON, conflict markers, nbformat 3): the raw text
        // is still worth reading.
        let (output, read) = match parsed {
            Ok(parsed) => parsed,
            Err(args) => return self.read.call(context, args).await.map(ToolOutput::text),
        };
        context.insert_result(FilesRead(vec![read]))?;
        Ok(ToolOutput::text(output))
    }
}

impl Tool for ReadView {
    const NAME: &'static str = "read";
    type Args = ReadArgs;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "{} An image (PNG, JPEG, GIF, WebP) is returned to the model as an image when the \
             model accepts images, and as a note otherwise; over {} MB even an image-capable \
             model gets a note. A Jupyter notebook (.ipynb) is shown as its cells: each with \
             its index, id and type, its source, and its text outputs (images are noted, not \
             shown). Edit one with `notebook_edit`. A PDF is never returned as a result: \
             attach it to your message instead.",
            self.read.description(),
            MAX_IMAGE_BYTES / (1024 * 1024),
        )
    }

    fn parameters(&self) -> serde_json::Value {
        self.read.parameters()
    }

    fn annotations(&self) -> ToolAnnotations {
        self.read.annotations()
    }

    fn footprint(&self, args: &ReadArgs) -> Touches {
        self.read.footprint(args)
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: ReadArgs,
    ) -> Result<ToolOutput, ToolExecutionError> {
        if is_notebook_path(&args.path) {
            return self.notebook(context, args).await;
        }
        // Resolved before any I/O, exactly as rig's `Read` resolves, so a confined path
        // outside the root is refused with the same error text.
        let path =
            self.read
                .workspace
                .place(self.read.base.as_deref(), self.read.confine, &args.path)?;
        let workspace = self.read.workspace.clone();
        let probe = path.clone();
        let (sniffed, size) = blocking(move || {
            let contents = workspace
                .read(&probe, 12)
                .map_err(|e| io_error(&probe.to_string_lossy(), e))?;
            Ok((sniff(&contents.bytes), contents.size))
        })
        .await?;
        match sniffed {
            Sniffed::Image(media) => self.image(context, &path, media, size).await,
            Sniffed::Pdf => Ok(self.pdf(&path, size)),
            Sniffed::Other => self.read.call(context, args).await.map(ToolOutput::text),
        }
    }
}

/// Appended to rig's descriptions so a model reaching for `edit` or `write` on a notebook is
/// pointed at the cell-level tool.
pub const NOTEBOOK_STEER: &str =
    " Do not use it on Jupyter notebooks (.ipynb); use `notebook_edit`.";

#[cfg(test)]
mod tests {
    use super::*;

    fn nb(minor: u64) -> String {
        format!(
            r##"{{
 "cells": [
  {{
   "cell_type": "markdown",
   "id": "intro",
   "metadata": {{"tags": ["x"]}},
   "source": "# Title\nbody"
  }},
  {{
   "cell_type": "code",
   "execution_count": 3,
   "id": "calc",
   "metadata": {{}},
   "outputs": [
    {{"name": "stdout", "output_type": "stream", "text": ["3\n"]}},
    {{"output_type": "execute_result", "execution_count": 3, "metadata": {{}}, "data": {{"text/plain": "3", "image/png": "AAAA"}}}},
    {{"output_type": "error", "ename": "ValueError", "evalue": "bad", "traceback": ["\u001b[31mValueError\u001b[0m: bad"]}}
   ],
   "source": ["a = 1\n", "a + 2"]
  }}
 ],
 "metadata": {{"custom": {{"keep": true}}}},
 "nbformat": 4,
 "nbformat_minor": {minor}
}}
"##
        )
    }

    fn args(
        mode: EditMode,
        id: Option<&str>,
        ty: Option<CellType>,
        src: Option<&str>,
    ) -> NotebookEditArgs {
        NotebookEditArgs {
            path: "n.ipynb".into(),
            cell_id: id.map(Into::into),
            cell_type: ty,
            edit_mode: mode,
            new_source: src.map(Into::into),
        }
    }

    fn parsed(minor: u64) -> Notebook {
        Notebook::parse(&nb(minor)).unwrap()
    }

    #[test]
    fn untouched_notebook_round_trips_byte_for_byte_shape() {
        let n = parsed(5);
        let again = Notebook::parse(&n.serialize()).unwrap();
        assert_eq!(n.root, again.root);
        assert_eq!(n.indent, 1);
        assert!(n.serialize().ends_with("}\n"));
        assert!(n.serialize().starts_with("{\n \"cells\""));
    }

    #[test]
    fn indent_and_missing_trailing_newline_are_kept() {
        for indent in [2usize, 4] {
            let pad = " ".repeat(indent);
            let text = format!("{{\n{pad}\"cells\": [],\n{pad}\"nbformat\": 4\n}}");
            let n = Notebook::parse(&text).unwrap();
            assert_eq!(n.serialize(), text);
        }
    }

    #[test]
    fn replace_clears_code_outputs_and_keeps_source_form_and_id() {
        let mut n = parsed(5);
        let e = n
            .apply(&args(
                EditMode::Replace,
                Some("calc"),
                None,
                Some("b = 2\nb"),
            ))
            .unwrap();
        assert_eq!(e.before.as_deref(), Some("a = 1\na + 2"));
        let cell = &n.cells()[1];
        assert_eq!(cell["outputs"], json!([]));
        assert_eq!(cell["execution_count"], Value::Null);
        assert_eq!(cell["id"], "calc");
        assert_eq!(cell["source"], json!(["b = 2\n", "b"]));
        n.apply(&args(EditMode::Replace, Some("intro"), None, Some("# New")))
            .unwrap();
        assert_eq!(n.cells()[0]["source"], "# New");
        assert_eq!(n.cells()[0]["metadata"], json!({"tags": ["x"]}));
    }

    #[test]
    fn replace_can_change_the_cell_type() {
        let mut n = parsed(5);
        n.apply(&args(
            EditMode::Replace,
            Some("calc"),
            Some(CellType::Markdown),
            Some("text"),
        ))
        .unwrap();
        let cell = &n.cells()[1];
        assert_eq!(cell["cell_type"], "markdown");
        assert!(cell.get("outputs").is_none() && cell.get("execution_count").is_none());
        assert_eq!(cell["source"], json!(["text"]));
        n.apply(&args(
            EditMode::Replace,
            Some("intro"),
            Some(CellType::Code),
            Some("x = 1"),
        ))
        .unwrap();
        let cell = &n.cells()[0];
        assert_eq!(cell["outputs"], json!([]));
        assert_eq!(cell["execution_count"], Value::Null);
    }

    #[test]
    fn cells_are_found_by_id_then_by_index() {
        let n = parsed(5);
        assert_eq!(n.find("calc"), Ok(1));
        assert_eq!(n.find("cell-0"), Ok(0));
        assert!(n.find("cell-2").unwrap_err().contains("no cell"));
        assert!(n.find("ghost").is_err());

        let mut n = parsed(5);
        n.cells_mut()[0]["id"] = json!("cell-1");
        assert_eq!(n.find("cell-1"), Ok(0), "an id wins over an index");
    }

    #[test]
    fn insert_goes_after_the_named_cell_or_at_the_start() {
        let mut n = parsed(5);
        let e = n
            .apply(&args(
                EditMode::Insert,
                Some("intro"),
                Some(CellType::Code),
                Some("x\ny\n"),
            ))
            .unwrap();
        assert_eq!(e.before, None);
        let cell = n.cells()[1].as_object().unwrap();
        assert_eq!(cell["source"], json!(["x\n", "y\n"]));
        assert_eq!(cell["outputs"], json!([]));
        assert_eq!(cell["metadata"], json!({}));
        let id = cell["id"].as_str().unwrap();
        assert_eq!(id.len(), 8);
        let keys: Vec<&String> = cell.keys().collect();
        assert_eq!(
            keys,
            [
                "cell_type",
                "execution_count",
                "id",
                "metadata",
                "outputs",
                "source"
            ]
        );

        n.apply(&args(
            EditMode::Insert,
            None,
            Some(CellType::Markdown),
            Some("top"),
        ))
        .unwrap();
        assert_eq!(n.cells()[0]["source"], json!(["top"]));
        assert!(n.cells()[0].get("outputs").is_none());
    }

    #[test]
    fn notebooks_before_4_5_get_no_ids() {
        let mut n = Notebook::parse(
            &nb(4)
                .replace("\"id\": \"intro\",", "")
                .replace("\"id\": \"calc\",", ""),
        )
        .unwrap();
        n.apply(&args(
            EditMode::Insert,
            Some("cell-0"),
            Some(CellType::Code),
            Some("1"),
        ))
        .unwrap();
        assert!(n.cells()[1].get("id").is_none());
        assert!(n.render().contains("[cell 1 type=code]"));
    }

    #[test]
    fn delete_removes_the_cell() {
        let mut n = parsed(5);
        let e = n
            .apply(&args(EditMode::Delete, Some("intro"), None, None))
            .unwrap();
        assert_eq!(e.after, None);
        assert_eq!(n.cells().len(), 1);
        assert_eq!(n.cells()[0]["id"], "calc");
    }

    #[test]
    fn unrelated_fields_and_key_order_survive() {
        let mut n = parsed(5);
        n.apply(&args(EditMode::Replace, Some("intro"), None, Some("z")))
            .unwrap();
        let out = n.serialize();
        assert!(out.contains("\"custom\": {\n   \"keep\": true\n  }"));
        let cell_type = out.find("\"cell_type\"").unwrap();
        let id = out.find("\"id\"").unwrap();
        let source = out.find("\"source\"").unwrap();
        assert!(cell_type < id && id < source);
    }

    #[test]
    fn errors_name_what_is_wrong() {
        assert!(Notebook::parse("{").unwrap_err().contains("not valid"));
        assert!(Notebook::parse("[]").unwrap_err().contains("not an object"));
        assert!(
            Notebook::parse("{\"a\":1}")
                .unwrap_err()
                .contains("no `cells`")
        );
        assert!(
            Notebook::parse("{\"cells\":[],\"nbformat\":3}")
                .unwrap_err()
                .contains("nbformat 3")
        );
        let mut n = parsed(5);
        let missing = n.apply(&args(EditMode::Replace, Some("ghost"), None, Some("x")));
        assert!(missing.unwrap_err().contains("ghost"));
        let no_source = n.apply(&args(EditMode::Replace, Some("calc"), None, None));
        assert!(no_source.unwrap_err().contains("new_source"));
        let no_type = n.apply(&args(EditMode::Insert, None, None, Some("x")));
        assert!(no_type.unwrap_err().contains("cell_type"));
        let no_cell = n.apply(&args(EditMode::Delete, None, None, None));
        assert!(no_cell.unwrap_err().contains("cell_id"));
        assert_eq!(n, parsed(5), "a refused edit changes nothing");
    }

    #[test]
    fn a_bad_cell_type_does_not_deserialize() {
        let bad = serde_json::from_value::<NotebookEditArgs>(
            json!({"path": "a.ipynb", "cell_type": "raw"}),
        );
        assert!(bad.is_err());
    }

    #[test]
    fn render_shows_cells_ids_and_text_outputs() {
        let out = parsed(5).render();
        assert!(
            out.starts_with("[cell 0 id=intro type=markdown]\n# Title\nbody\n"),
            "{out}"
        );
        assert!(out.contains("[cell 1 id=calc type=code]\na = 1\na + 2\n"));
        assert!(out.contains("[output: stdout]\n3\n"));
        assert!(out.contains("[output: execute_result]\n3\n[image/png output omitted]\n"));
        assert!(
            out.contains("[output: error ValueError: bad]\nValueError: bad\n"),
            "{out}"
        );
        assert!(!out.contains('\u{1b}'));
    }

    #[test]
    fn only_ipynb_paths_are_notebooks() {
        assert!(is_notebook_path("a/b.ipynb"));
        assert!(is_notebook_path("B.IPYNB"));
        assert!(!is_notebook_path("a.json"));
        assert!(!is_notebook_path("ipynb"));
    }

    #[test]
    fn cell_change_gives_the_source_before_and_after() {
        let call = json!({"path": "n.ipynb", "cell_id": "intro", "new_source": "x"});
        let e = cell_change(&nb(5), &call).unwrap();
        assert_eq!(e.before.as_deref(), Some("# Title\nbody"));
        assert_eq!(e.after.as_deref(), Some("x"));
        assert!(cell_change("nope", &call).is_none());
        assert!(
            cell_change(
                &nb(5),
                &json!({"path": "n.ipynb", "cell_id": "ghost", "new_source": "x"})
            )
            .is_none()
        );
    }

    mod tools {
        use super::*;
        use crate::gate::InputModalities;
        use rig_core::catalog::Modalities;
        use rig_core::message::DocumentSourceKind;
        use rig_tools::MAX_FILE_BYTES;

        /// The smallest file each magic-byte sniff recognizes.
        const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";
        const JPEG: &[u8] = b"\xff\xd8\xff\xe0rest";
        const GIF: &[u8] = b"GIF89arest";
        const WEBP: &[u8] = b"RIFF\0\0\0\0WEBPrest";
        const PDF: &[u8] = b"%PDF-1.7 rest";

        fn modalities(image: bool) -> ToolContext {
            let mut context = ToolContext::new();
            let mut accepted = Modalities::default();
            accepted.image = image;
            context
                .insert(InputModalities(accepted))
                .expect("modalities serialize");
            context
        }

        fn text_of(output: &ToolOutput) -> String {
            crate::result_pipeline::text_view(output)
        }

        struct Rig {
            _dir: tempfile::TempDir,
            root: PathBuf,
            seen: Seen,
        }

        impl Rig {
            fn new() -> Rig {
                let dir = tempfile::tempdir().unwrap();
                let root = std::fs::canonicalize(dir.path()).unwrap();
                Rig {
                    _dir: dir,
                    root,
                    seen: Seen::default(),
                }
            }

            fn write(&self, name: &str, text: &str) {
                std::fs::write(self.root.join(name), text).unwrap();
            }

            fn write_bytes(&self, name: &str, bytes: &[u8]) {
                std::fs::write(self.root.join(name), bytes).unwrap();
            }

            fn read_tool(&self) -> ReadView {
                ReadView {
                    read: crate::builtins::read_for(&self.root, &self.seen),
                }
            }

            fn edit_tool(&self) -> NotebookEdit {
                crate::builtins::notebook_edit_for(&self.root, &self.seen)
            }

            async fn read(&self, path: &str) -> Result<ToolOutput, ToolExecutionError> {
                self.read_tool()
                    .call(&mut ToolContext::new(), ReadArgs::new(path))
                    .await
            }

            async fn read_with(
                &self,
                path: &str,
                context: &mut ToolContext,
            ) -> Result<ToolOutput, ToolExecutionError> {
                self.read_tool().call(context, ReadArgs::new(path)).await
            }

            async fn edit(&self, a: NotebookEditArgs) -> Result<String, ToolExecutionError> {
                self.edit_tool().call(&mut ToolContext::new(), a).await
            }
        }

        fn replace(path: &str) -> NotebookEditArgs {
            NotebookEditArgs {
                path: path.into(),
                ..args(EditMode::Replace, Some("calc"), None, Some("new"))
            }
        }

        #[test]
        fn the_sniff_reads_magic_bytes_only() {
            assert!(matches!(sniff(PNG), Sniffed::Image(ImageMediaType::PNG)));
            assert!(matches!(sniff(JPEG), Sniffed::Image(ImageMediaType::JPEG)));
            assert!(matches!(sniff(GIF), Sniffed::Image(ImageMediaType::GIF)));
            assert!(matches!(sniff(WEBP), Sniffed::Image(ImageMediaType::WEBP)));
            assert!(matches!(sniff(PDF), Sniffed::Pdf));
            assert!(matches!(sniff(b"plain text"), Sniffed::Other));
            assert!(matches!(sniff(b""), Sniffed::Other));
            assert!(matches!(sniff(b"%PDF"), Sniffed::Other), "the dash matters");
        }

        #[test]
        fn sizes_read_the_way_headers_name_them() {
            assert_eq!(human_size(512), "512 B");
            assert_eq!(human_size(1024), "1 KB");
            assert_eq!(human_size(231_000), "226 KB");
            assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
        }

        #[tokio::test]
        async fn an_image_reads_as_parts_for_a_model_that_accepts_images() {
            let rig = Rig::new();
            rig.write_bytes("pixel.png", PNG);
            let mut context = modalities(true);
            let output = rig.read_with("pixel.png", &mut context).await.unwrap();
            let parts = output.as_content();
            assert_eq!(parts.len(), 2);
            let shown = text_of(&output);
            assert!(shown.contains("[image: PNG"), "{shown}");
            assert!(shown.contains("pixel.png"), "{shown}");
            assert!(!shown.contains("iVBOR"), "no base64 in the text view");
            let ToolResultContent::Image(image) = &parts[1] else {
                panic!("the second part is the image");
            };
            assert_eq!(image.media_type, Some(ImageMediaType::PNG));
            let DocumentSourceKind::Base64(data) = &image.data else {
                panic!("the image travels as base64");
            };
            assert_eq!(STANDARD.decode(data).unwrap(), PNG);
            let read = context
                .result::<FilesRead>()
                .unwrap()
                .expect("the read is published");
            assert_eq!(read.0.len(), 1);
        }

        #[tokio::test]
        async fn an_image_without_modalities_reads_as_a_text_note() {
            let rig = Rig::new();
            rig.write_bytes("pixel.png", PNG);
            // No InputModalities in the context at all: the text-only default.
            let output = rig.read("pixel.png").await.unwrap();
            let shown = text_of(&output);
            assert!(shown.contains("is an image"), "{shown}");
            assert!(shown.contains("does not accept images"), "{shown}");
            assert!(!shown.contains("iVBOR"), "{shown}");
        }

        #[tokio::test]
        async fn an_image_for_a_text_only_model_reads_as_a_text_note() {
            let rig = Rig::new();
            rig.write_bytes("pixel.png", PNG);
            let mut context = modalities(false);
            let output = rig.read_with("pixel.png", &mut context).await.unwrap();
            let shown = text_of(&output);
            assert!(shown.contains("does not accept images"), "{shown}");
        }

        #[tokio::test]
        async fn an_image_over_the_cap_is_not_sent() {
            let rig = Rig::new();
            let mut big = PNG.to_vec();
            big.resize((MAX_IMAGE_BYTES + 1) as usize, 0);
            rig.write_bytes("big.png", &big);
            let mut context = modalities(true);
            let output = rig.read_with("big.png", &mut context).await.unwrap();
            let shown = text_of(&output);
            assert!(shown.contains("over the 5 MB limit"), "{shown}");
            assert!(!shown.contains("iVBOR"), "{shown}");
            assert_eq!(output.as_content().len(), 1, "a text-only result");
        }

        #[tokio::test]
        async fn a_pdf_reads_as_a_stub_pointing_at_attach() {
            let rig = Rig::new();
            rig.write_bytes("doc.pdf", PDF);
            let mut context = modalities(true);
            let output = rig.read_with("doc.pdf", &mut context).await.unwrap();
            let shown = text_of(&output);
            assert!(shown.contains("doc.pdf"), "{shown}");
            assert!(shown.contains("PDF"), "{shown}");
            assert!(shown.contains("attach"), "{shown}");
            assert!(!shown.contains("JVBER"), "no base64 in the stub");
        }

        #[tokio::test]
        async fn an_image_outside_the_root_is_refused_like_any_read() {
            let rig = Rig::new();
            let outside = std::env::temp_dir().join(format!(
                "fontaine-readview-test-{}-outside.png",
                uuid::Uuid::new_v4()
            ));
            std::fs::write(&outside, PNG).unwrap();
            let outside = outside.to_string_lossy().into_owned();
            // The unconfined fallback read, as the gate's external fallback builds it.
            let tool = ReadView {
                read: crate::builtins::read_external(&rig.root, &rig.seen),
            };
            let mut context = modalities(true);
            let output = tool
                .call(&mut context, ReadArgs::new(&outside))
                .await
                .unwrap();
            assert!(text_of(&output).contains("[image: PNG"));
            // A confined read of the same path is refused before any I/O, with the same
            // error text rig's own `Read` refuses it with.
            let err = rig.read(&outside).await.unwrap_err();
            let plain = crate::builtins::read_for(&rig.root, &rig.seen);
            let err_plain = plain
                .call(&mut ToolContext::new(), ReadArgs::new(&outside))
                .await
                .unwrap_err();
            assert_eq!(err.to_string(), err_plain.to_string());
            assert!(err.to_string().contains("outside"), "{err}");
            std::fs::remove_file(&outside).unwrap();
        }

        #[tokio::test]
        async fn reading_renders_cells_and_editing_writes_them_back() {
            let rig = Rig::new();
            rig.write("n.ipynb", &nb(5));
            let shown = text_of(&rig.read("n.ipynb").await.unwrap());
            assert!(shown.contains("[cell 1 id=calc type=code]"), "{shown}");
            let reply = rig.edit(replace("n.ipynb")).await.unwrap();
            assert!(reply.contains("Replaced the source of cell 1"), "{reply}");
            let disk = std::fs::read_to_string(rig.root.join("n.ipynb")).unwrap();
            let n = Notebook::parse(&disk).unwrap();
            assert_eq!(n.cells()[1]["source"], json!(["new"]));
            assert_eq!(n.cells()[1]["outputs"], json!([]));
            // The edit leaves the file as seen, so a second edit needs no new read.
            rig.edit(replace("n.ipynb")).await.unwrap();
        }

        #[tokio::test]
        async fn a_notebook_not_read_first_is_refused() {
            let rig = Rig::new();
            rig.write("n.ipynb", &nb(5));
            let err = rig.edit(replace("n.ipynb")).await.unwrap_err();
            assert!(err.to_string().contains("read"), "{err}");
            assert_eq!(
                std::fs::read_to_string(rig.root.join("n.ipynb")).unwrap(),
                nb(5)
            );
        }

        #[tokio::test]
        async fn a_notebook_changed_after_reading_is_refused_untouched() {
            let rig = Rig::new();
            rig.write("n.ipynb", &nb(5));
            rig.read("n.ipynb").await.unwrap();
            let other = nb(5).replace("# Title", "# Other");
            rig.write("n.ipynb", &other);
            let err = rig.edit(replace("n.ipynb")).await.unwrap_err();
            assert!(err.to_string().contains("changed since"), "{err}");
            assert_eq!(
                std::fs::read_to_string(rig.root.join("n.ipynb")).unwrap(),
                other
            );
        }

        #[tokio::test]
        async fn paths_outside_the_root_and_non_notebooks_are_refused() {
            let rig = Rig::new();
            let outside = rig.edit(replace("../x.ipynb")).await;
            assert!(outside.is_err());
            rig.write("a.json", "{}");
            let wrong = rig.edit(replace("a.json")).await.unwrap_err();
            assert!(wrong.to_string().contains(".ipynb"), "{wrong}");
        }

        #[tokio::test]
        async fn a_notebook_over_the_seen_limit_counts_as_not_read() {
            let rig = Rig::new();
            let padding = "x".repeat(8 * 1024 * 1024 + 1);
            let big = nb(5).replace("body", &padding);
            assert!(big.len() as u64 <= MAX_FILE_BYTES);
            rig.write("big.ipynb", &big);
            rig.read("big.ipynb").await.unwrap();
            let err = rig.edit(replace("big.ipynb")).await.unwrap_err();
            assert!(err.to_string().contains("8 MiB"), "{err}");
            assert_eq!(
                std::fs::read_to_string(rig.root.join("big.ipynb")).unwrap(),
                big
            );
        }

        #[tokio::test]
        async fn other_files_read_as_rig_reads_them() {
            let rig = Rig::new();
            rig.write("a.txt", "one\ntwo\n");
            assert!(text_of(&rig.read("a.txt").await.unwrap()).contains("one"));
            rig.write("bad.ipynb", "not json");
            // Not an editable notebook: read as plain text rather than refused.
            assert!(text_of(&rig.read("bad.ipynb").await.unwrap()).contains("not json"));
            rig.write("v3.ipynb", r#"{"nbformat": 3, "worksheets": []}"#);
            assert!(text_of(&rig.read("v3.ipynb").await.unwrap()).contains("worksheets"));
        }

        #[tokio::test]
        async fn editing_publishes_the_change() {
            let rig = Rig::new();
            rig.write("n.ipynb", &nb(5));
            rig.read("n.ipynb").await.unwrap();
            let mut context = ToolContext::new();
            rig.edit_tool()
                .call(&mut context, replace("n.ipynb"))
                .await
                .unwrap();
            let changed = context.result::<FilesChanged>().unwrap();
            assert!(changed.is_some_and(|c| c.0.len() == 1));
        }
    }
}
