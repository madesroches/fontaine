//! The status line as a Rune script. The script is handed a `ctx` over a snapshot of the session
//! and returns the line; the host owns every constructor, so the script can only build a line
//! out of runs the host made.
//!
//! The posture is the guardian composition's: no stdio, a host module of opaque types, an
//! instruction budget around the call and an entry-point arity check at compile time. Unlike the
//! composition, a render is synchronous and per frame, so its budget is small and a failure
//! falls back to the embedded default program rather than denying anything.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use rune::runtime::{RuntimeContext, Value, VmResult, budget};
use rune::termcolor::NoColor;
use rune::{Any, Context, ContextError, Diagnostics, Module, Source, Sources, Unit, Vm};

use super::snapshot::{Role, StatuslineSnapshot, StyledText};
use crate::core::goal::duration_text;

/// The most instructions one render may run: an order of magnitude above a full render of the
/// default line, so the worst case stays far under a frame.
pub const STATUSLINE_BUDGET: usize = 50_000;

/// The program that renders the line when no script is configured, and the fallback when one
/// fails.
pub const DEFAULT_SCRIPT: &str = include_str!("default.rn");

/// A script that cannot be compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatuslineError {
    message: String,
}

impl StatuslineError {
    fn new(message: impl Into<String>) -> Self {
        StatuslineError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StatuslineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StatuslineError {}

/// A compiled status-line script.
#[derive(Clone)]
pub struct StatuslineProgram {
    unit: Arc<Unit>,
    runtime: Arc<RuntimeContext>,
}

impl std::fmt::Debug for StatuslineProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StatuslineProgram")
    }
}

impl StatuslineProgram {
    /// Compiles `source` against the host module and requires a `statusline` entry point of one
    /// parameter. It never runs the script, so a runtime error is not found here.
    pub fn compile(source: &str) -> Result<StatuslineProgram, StatuslineError> {
        let fail = |what: &str, error: &dyn std::fmt::Display| {
            StatuslineError::new(format!("{what}: {error}"))
        };
        // Without stdio, a script cannot write to the terminal the interface owns.
        let mut context =
            Context::with_config(false).map_err(|e| fail("building the script context", &e))?;
        context
            .install(host_module().map_err(|e| fail("building the host module", &e))?)
            .map_err(|e| fail("installing the host module", &e))?;
        let mut sources = Sources::new();
        sources
            .insert(Source::memory(source).map_err(|e| fail("reading the script", &e))?)
            .map_err(|e| fail("reading the script", &e))?;
        let mut diagnostics = Diagnostics::new();
        let built = rune::prepare(&mut sources)
            .with_context(&context)
            .with_diagnostics(&mut diagnostics)
            .build();
        let unit = match built {
            Ok(unit) if !diagnostics.has_error() => unit,
            _ => {
                let mut text = NoColor::new(Vec::new());
                let rendered = diagnostics.emit(&mut text, &sources).map(|()| {
                    String::from_utf8_lossy(&text.into_inner())
                        .trim()
                        .to_owned()
                });
                return Err(StatuslineError::new(format!(
                    "the script does not compile: {}",
                    rendered.unwrap_or_else(|e| e.to_string())
                )));
            }
        };
        let takes_one = unit.debug_info().is_some_and(|debug| {
            debug.functions.values().any(|signature| {
                signature.path.to_string() == "statusline"
                    && match &signature.args {
                        rune::runtime::debug::DebugArgs::TupleArgs(n) => *n == 1,
                        rune::runtime::debug::DebugArgs::Named(names) => names.len() == 1,
                        rune::runtime::debug::DebugArgs::EmptyArgs => false,
                    }
            })
        });
        if !takes_one {
            return Err(StatuslineError::new(
                "the script must define `pub fn statusline(ctx)`",
            ));
        }
        let runtime = context
            .runtime()
            .map_err(|e| fail("building the script runtime", &e))?;
        Ok(StatuslineProgram {
            unit: Arc::new(unit),
            runtime: Arc::new(runtime),
        })
    }

    /// Runs the script over `snapshot` and returns the runs of its line. A runtime error, a
    /// panic or an exhausted instruction budget is an `Err` carrying why.
    ///
    /// A host panic is caught here, which only keeps the session alive when the caller has told
    /// the terminal's panic hook to leave the terminal alone for the duration.
    pub fn render(&self, snapshot: &StatuslineSnapshot) -> Result<Vec<StyledText>, String> {
        let ctx = Ctx {
            snapshot: Arc::new(snapshot.clone()),
        };
        let mut vm = Vm::new(self.runtime.clone(), self.unit.clone());
        let called = catch_unwind(AssertUnwindSafe(|| {
            budget::with(STATUSLINE_BUDGET, || vm.call(["statusline"], (ctx,))).call()
        }));
        let value = match called {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => return Err(format!("the status line script failed: {error}")),
            Err(_) => return Err("the status line script panicked".to_owned()),
        };
        let mut runs = runs_of(&value)?;
        // Control characters would move the cursor or start an escape sequence in the row.
        for run in &mut runs {
            run.text.retain(|c| !c.is_control());
        }
        Ok(runs)
    }
}

/// The runs a script's value is: a line, a fragment, or a string drawn in the normal style.
fn runs_of(value: &Value) -> Result<Vec<StyledText>, String> {
    if let Ok(fragment) = value.borrow_ref::<Fragment>() {
        return Ok(fragment.runs.clone());
    }
    if let Ok(line) = value.borrow_ref::<Line>() {
        return Ok(line.runs.clone());
    }
    if let Ok(text) = value.borrow_string_ref() {
        return Ok(vec![StyledText::new(text.to_string(), Role::Normal)]);
    }
    Err(format!(
        "the status line script returned {} instead of a line, a fragment or a string",
        value.type_info()
    ))
}

/// One piece of a template.
#[derive(Debug, PartialEq, Eq)]
enum Piece {
    Literal(String),
    Name(String),
}

/// Splits a template into literals and `{name}` placeholders. `{{` and `}}` are a literal brace.
fn parse_template(template: &str) -> Result<Vec<Piece>, String> {
    let mut pieces = Vec::new();
    let mut literal = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some('{') | None => {
                            return Err(format!(
                                "the template has a `{{` with no closing `}}` after `{name}`"
                            ));
                        }
                        Some(c) => name.push(c),
                    }
                }
                if name.is_empty() {
                    return Err("the template has an empty `{}`".to_owned());
                }
                if !literal.is_empty() {
                    pieces.push(Piece::Literal(std::mem::take(&mut literal)));
                }
                pieces.push(Piece::Name(name));
            }
            '}' => return Err("the template has a `}` with no opening `{`".to_owned()),
            c => literal.push(c),
        }
    }
    if !literal.is_empty() {
        pieces.push(Piece::Literal(literal));
    }
    Ok(pieces)
}

/// What a name expands to: its segment's runs, or its value as one dim run. Runs with no text
/// are dropped, so a name with nothing to show expands to nothing.
fn lookup(snapshot: &StatuslineSnapshot, name: &str) -> Result<Vec<StyledText>, String> {
    let runs = if let Some(runs) = snapshot.segment(name) {
        runs.to_vec()
    } else if let Some(value) = snapshot.values.text(name) {
        value
            .into_iter()
            .map(|text| StyledText::new(text, Role::Dim))
            .collect()
    } else {
        return Err(format!(
            "`{name}` is not a status line segment or value; the names are listed in the documentation"
        ));
    };
    Ok(runs
        .into_iter()
        .filter(|run| !run.text.is_empty())
        .collect())
}

/// A run while a template is being expanded, with whether it is a literal `·` separator.
struct Expanded {
    run: StyledText,
    separator: bool,
}

/// Expands a template into runs: placeholders become their runs and literals become dim runs. A
/// literal of only spaces before a placeholder that expands to nothing is dropped with it, and
/// the separators `·` are then normalised so that an empty piece takes one with it.
fn expand(snapshot: &StatuslineSnapshot, template: &str) -> Result<Vec<StyledText>, String> {
    enum Resolved {
        Literal(String),
        Runs(Vec<StyledText>),
    }
    let resolved = parse_template(template)?
        .into_iter()
        .map(|piece| match piece {
            Piece::Literal(text) => Ok(Resolved::Literal(text)),
            Piece::Name(name) => lookup(snapshot, &name).map(Resolved::Runs),
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut runs = Vec::new();
    for (i, piece) in resolved.iter().enumerate() {
        match piece {
            Resolved::Literal(text) => {
                let padding = text.chars().all(|c| c == ' ');
                let before_nothing =
                    matches!(resolved.get(i + 1), Some(Resolved::Runs(runs)) if runs.is_empty());
                if padding && before_nothing {
                    continue;
                }
                runs.push(Expanded {
                    run: StyledText::new(text.clone(), Role::Dim),
                    separator: text.trim() == "·",
                });
            }
            Resolved::Runs(expanded) => runs.extend(expanded.iter().cloned().map(|run| Expanded {
                run,
                separator: false,
            })),
        }
    }
    Ok(normalise(runs))
}

/// Drops empty runs, collapses each group of adjacent separators to its first, and drops a
/// separator that is first or last.
fn normalise(runs: Vec<Expanded>) -> Vec<StyledText> {
    let mut kept: Vec<Expanded> = Vec::new();
    for run in runs.into_iter().filter(|run| !run.run.text.is_empty()) {
        if run.separator && kept.last().is_some_and(|last| last.separator) {
            continue;
        }
        kept.push(run);
    }
    if kept.last().is_some_and(|last| last.separator) {
        kept.pop();
    }
    if kept.first().is_some_and(|first| first.separator) {
        kept.remove(0);
    }
    kept.into_iter().map(|run| run.run).collect()
}

/// What the script is handed. It reads the snapshot and builds lines; it holds no state.
#[derive(Any, Clone)]
#[rune(item = ::Ctx)]
struct Ctx {
    snapshot: Arc<StatuslineSnapshot>,
}

impl Ctx {
    #[rune::function]
    fn mode(&self) -> String {
        self.snapshot.mode.clone()
    }

    #[rune::function]
    fn model(&self) -> String {
        self.snapshot.model.clone()
    }

    #[rune::function]
    fn dir(&self) -> String {
        self.snapshot.dir.clone()
    }

    #[rune::function]
    fn branch(&self) -> Option<String> {
        self.snapshot.branch.clone()
    }

    #[rune::function]
    fn cwd(&self) -> String {
        self.snapshot.cwd.clone()
    }

    #[rune::function]
    fn project_dir(&self) -> String {
        self.snapshot.project_dir.clone()
    }

    #[rune::function]
    fn session_id(&self) -> String {
        self.snapshot.session_id.clone()
    }

    #[rune::function]
    fn version(&self) -> String {
        self.snapshot.version.clone()
    }

    #[rune::function]
    fn effort(&self) -> Option<String> {
        self.snapshot.effort.clone()
    }

    #[rune::function]
    fn vim(&self) -> Option<String> {
        self.snapshot.vim.clone()
    }

    #[rune::function]
    fn afk(&self) -> bool {
        self.snapshot.afk
    }

    #[rune::function]
    fn afk_stopped(&self) -> bool {
        self.snapshot.afk_stopped
    }

    #[rune::function]
    fn guardian_off(&self) -> bool {
        self.snapshot.guardian_off
    }

    #[rune::function]
    fn running_agents(&self) -> usize {
        self.snapshot.running_agents
    }

    /// The running background shells and monitors.
    #[rune::function]
    fn background_jobs(&self) -> (usize, usize) {
        (
            self.snapshot.background_shells,
            self.snapshot.background_monitors,
        )
    }

    #[rune::function]
    fn scheduled(&self) -> usize {
        self.snapshot.scheduled
    }

    #[rune::function]
    fn bug_drafts(&self) -> Option<usize> {
        self.snapshot.bug_drafts
    }

    /// `paused`, or the goal's age as `4m`.
    #[rune::function]
    fn goal(&self) -> Option<String> {
        self.snapshot.goal.map(|(age, paused)| {
            if paused {
                "paused".to_owned()
            } else {
                duration_text(std::time::Duration::from_secs(age))
            }
        })
    }

    #[rune::function]
    fn remote(&self) -> Option<String> {
        self.snapshot.remote.clone()
    }

    #[rune::function]
    fn context_used_percentage(&self) -> Option<u32> {
        self.snapshot.values.context_used_percentage
    }

    #[rune::function]
    fn context_remaining_percentage(&self) -> Option<u32> {
        self.snapshot.values.context_remaining_percentage
    }

    #[rune::function]
    fn context_total_tokens(&self) -> Option<u64> {
        self.snapshot.values.context_total_tokens
    }

    #[rune::function]
    fn context_window_size(&self) -> Option<u32> {
        self.snapshot.values.context_window_size
    }

    #[rune::function]
    fn cost_total_cost_usd(&self) -> Option<f64> {
        self.snapshot.values.cost_total_cost_usd
    }

    #[rune::function]
    fn cost_is_estimate(&self) -> bool {
        self.snapshot.cost_is_estimate
    }

    #[rune::function]
    fn command_line(&self) -> Option<String> {
        self.snapshot.command_line.clone()
    }

    /// The template's runs. A name that is neither a segment nor a value fails the render.
    #[rune::function]
    fn format(&self, template: &str) -> VmResult<Fragment> {
        match expand(&self.snapshot, template) {
            Ok(runs) => VmResult::Ok(Fragment { runs }),
            Err(message) => VmResult::panic(message),
        }
    }

    #[rune::function]
    fn segment(&self, name: &str) -> VmResult<Fragment> {
        match lookup(&self.snapshot, name) {
            Ok(runs) => VmResult::Ok(Fragment { runs }),
            Err(message) => VmResult::panic(message),
        }
    }

    #[rune::function]
    fn style(&self, name: &str) -> VmResult<Style> {
        match Role::named(name) {
            Some(role) => VmResult::Ok(Style { role }),
            None => VmResult::panic(format!("`{name}` is not a status line style")),
        }
    }

    #[rune::function]
    fn line(&self) -> Line {
        Line { runs: Vec::new() }
    }
}

/// Styled runs built by `ctx.format` or `ctx.segment`.
#[derive(Any, Clone)]
#[rune(item = ::Fragment)]
struct Fragment {
    runs: Vec<StyledText>,
}

/// A role a run is drawn in, from `ctx.style`.
#[derive(Any, Clone, Copy)]
#[rune(item = ::Style)]
struct Style {
    role: Role,
}

/// The line a script builds up.
#[derive(Any, Clone)]
#[rune(item = ::Line)]
struct Line {
    runs: Vec<StyledText>,
}

impl Line {
    #[rune::function]
    fn push(&mut self, fragment: &Fragment) {
        self.runs.extend(fragment.runs.iter().cloned());
    }

    #[rune::function]
    fn push_text(&mut self, text: &str) {
        self.runs.push(StyledText::new(text, Role::Normal));
    }

    #[rune::function]
    fn push_styled(&mut self, text: &str, style: &Style) {
        self.runs.push(StyledText::new(text, style.role));
    }
}

/// The types are in scope of every script without an import.
fn host_module() -> Result<Module, ContextError> {
    let mut module = Module::new();
    module.ty::<Ctx>()?;
    module.ty::<Fragment>()?;
    module.ty::<Style>()?;
    module.ty::<Line>()?;
    module.function_meta(Ctx::mode)?;
    module.function_meta(Ctx::model)?;
    module.function_meta(Ctx::dir)?;
    module.function_meta(Ctx::branch)?;
    module.function_meta(Ctx::cwd)?;
    module.function_meta(Ctx::project_dir)?;
    module.function_meta(Ctx::session_id)?;
    module.function_meta(Ctx::version)?;
    module.function_meta(Ctx::effort)?;
    module.function_meta(Ctx::vim)?;
    module.function_meta(Ctx::afk)?;
    module.function_meta(Ctx::afk_stopped)?;
    module.function_meta(Ctx::guardian_off)?;
    module.function_meta(Ctx::running_agents)?;
    module.function_meta(Ctx::background_jobs)?;
    module.function_meta(Ctx::scheduled)?;
    module.function_meta(Ctx::bug_drafts)?;
    module.function_meta(Ctx::goal)?;
    module.function_meta(Ctx::remote)?;
    module.function_meta(Ctx::context_used_percentage)?;
    module.function_meta(Ctx::context_remaining_percentage)?;
    module.function_meta(Ctx::context_total_tokens)?;
    module.function_meta(Ctx::context_window_size)?;
    module.function_meta(Ctx::cost_total_cost_usd)?;
    module.function_meta(Ctx::cost_is_estimate)?;
    module.function_meta(Ctx::command_line)?;
    module.function_meta(Ctx::format)?;
    module.function_meta(Ctx::segment)?;
    module.function_meta(Ctx::style)?;
    module.function_meta(Ctx::line)?;
    module.function_meta(Line::push)?;
    module.function_meta(Line::push_text)?;
    module.function_meta(Line::push_styled)?;
    #[cfg(test)]
    module
        .function("host_panic", || -> bool {
            panic!("a host function panicked")
        })
        .build()?;
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::statusline::snapshot::{SEGMENTS, VALUES, Values};

    fn snapshot(segments: &[(&'static str, &str)]) -> StatuslineSnapshot {
        let mut snapshot = StatuslineSnapshot::default();
        for (name, text) in segments {
            snapshot
                .segments
                .insert(name, vec![StyledText::new(*text, Role::Dim)]);
        }
        snapshot
    }

    fn text(runs: &[StyledText]) -> String {
        runs.iter().map(|run| run.text.as_str()).collect()
    }

    fn render(source: &str, snapshot: &StatuslineSnapshot) -> Result<Vec<StyledText>, String> {
        StatuslineProgram::compile(source)
            .expect("the test script compiles")
            .render(snapshot)
    }

    fn pieces(template: &str) -> Vec<Piece> {
        parse_template(template).expect("a valid template")
    }

    fn lit(text: &str) -> Piece {
        Piece::Literal(text.to_owned())
    }

    fn name(text: &str) -> Piece {
        Piece::Name(text.to_owned())
    }

    #[test]
    fn a_template_splits_into_placeholders_and_literals() {
        assert_eq!(
            pieces("a {mode} · {quota.guardian}!"),
            [
                lit("a "),
                name("mode"),
                lit(" · "),
                name("quota.guardian"),
                lit("!")
            ]
        );
        assert_eq!(pieces(""), []);
        assert_eq!(pieces("{mode}"), [name("mode")]);
    }

    #[test]
    fn doubled_braces_are_literal_braces() {
        assert_eq!(
            pieces("{{mode}} {mode} }}{{"),
            [lit("{mode} "), name("mode"), lit(" }{")]
        );
    }

    #[test]
    fn a_malformed_template_is_refused() {
        for template in ["{mode", "mode}", "{}", "{mo{de}", "{{mode}"] {
            assert!(parse_template(template).is_err(), "{template}");
        }
    }

    #[test]
    fn a_placeholder_appends_its_runs_and_a_literal_appends_dim_text() {
        let mut snapshot = snapshot(&[("model", "m")]);
        snapshot.segments.insert(
            "mode",
            vec![
                StyledText::new("man", Role::ModeManual),
                StyledText::new("ual", Role::Bold),
            ],
        );
        let runs = expand(&snapshot, "[{mode}] {model}").unwrap();
        assert_eq!(
            runs,
            [
                StyledText::new("[", Role::Dim),
                StyledText::new("man", Role::ModeManual),
                StyledText::new("ual", Role::Bold),
                StyledText::new("] ", Role::Dim),
                StyledText::new("m", Role::Dim),
            ]
        );
    }

    #[test]
    fn a_value_expands_to_its_plain_number_and_to_nothing_when_unknown() {
        let mut snapshot = snapshot(&[]);
        assert_eq!(
            text(&expand(&snapshot, "x {context.used_percentage} y").unwrap()),
            "x  y"
        );
        snapshot.values = Values {
            context_used_percentage: Some(42),
            cost_total_cost_usd: Some(0.5),
            ..Values::default()
        };
        assert_eq!(
            expand(&snapshot, "{context.used_percentage}|{cost.total_cost_usd}").unwrap(),
            [
                StyledText::new("42", Role::Dim),
                StyledText::new("|", Role::Dim),
                StyledText::new("0.5000", Role::Dim),
            ]
        );
    }

    #[test]
    fn an_unknown_name_is_an_error_naming_it() {
        let error = expand(&snapshot(&[]), "{mode} {nope}").unwrap_err();
        assert!(error.contains("`nope`"), "{error}");
    }

    #[test]
    fn spaces_before_an_empty_placeholder_go_with_it() {
        let snapshot = snapshot(&[("mode", "manual")]);
        assert_eq!(
            text(&expand(&snapshot, " {mode} {vim}").unwrap()),
            " manual"
        );
        let snapshot = self::snapshot(&[("mode", "manual"), ("vim", "vim:insert")]);
        assert_eq!(
            text(&expand(&snapshot, " {mode} {vim}").unwrap()),
            " manual vim:insert"
        );
        // Only a literal of spaces goes: other text stays whatever follows it.
        let snapshot = self::snapshot(&[("mode", "manual")]);
        assert_eq!(
            text(&expand(&snapshot, "{mode} at {vim}").unwrap()),
            "manual at "
        );
    }

    /// `{a} · {b} · {c}` with each of the eight combinations of present and absent pieces.
    #[test]
    fn a_separator_goes_with_an_empty_neighbour() {
        let cases = [
            ("abc", "a · b · c"),
            ("ab_", "a · b"),
            ("a_c", "a · c"),
            ("a__", "a"),
            ("_bc", "b · c"),
            ("_b_", "b"),
            ("__c", "c"),
            ("___", ""),
        ];
        for (present, expected) in cases {
            let mut snapshot = StatuslineSnapshot::default();
            for (flag, (name, text)) in
                present
                    .chars()
                    .zip([("mode", "a"), ("vim", "b"), ("model", "c")])
            {
                if flag != '_' {
                    snapshot
                        .segments
                        .insert(name, vec![StyledText::new(text, Role::Dim)]);
                }
            }
            let runs = expand(&snapshot, "{mode} · {vim} · {model}").unwrap();
            assert_eq!(text(&runs), expected, "{present}");
        }
    }

    #[test]
    fn leading_trailing_and_adjacent_separators_are_dropped() {
        let snapshot = snapshot(&[("mode", "a")]);
        assert_eq!(text(&expand(&snapshot, " · {mode}").unwrap()), "a");
        assert_eq!(text(&expand(&snapshot, "· {mode} ·").unwrap()), "a");
        assert_eq!(text(&expand(&snapshot, "·").unwrap()), "");
        assert_eq!(text(&expand(&snapshot, "{mode}·{mode}").unwrap()), "a·a");
        // Adjacent separators meet only around an empty piece.
        assert_eq!(
            text(&expand(&snapshot, "{mode} · {vim} · {model} · {mode}").unwrap()),
            "a · a"
        );
        // A literal that is more than the one separator is plain text.
        assert_eq!(
            text(&expand(&snapshot, "{mode} · · {mode}").unwrap()),
            "a · · a"
        );
    }

    #[test]
    fn the_empty_location_reads_without_gluing_its_neighbours() {
        let snapshot = snapshot(&[("mode", "manual"), ("model", "m")]);
        assert_eq!(
            text(&expand(&snapshot, "{mode} · {location} · {model}").unwrap()),
            "manual · m"
        );
    }

    #[test]
    fn a_string_a_line_and_a_fragment_are_each_a_line() {
        let snapshot = snapshot(&[("mode", "manual")]);
        let string = render(r#"pub fn statusline(ctx) { "plain" }"#, &snapshot).unwrap();
        assert_eq!(string, [StyledText::new("plain", Role::Normal)]);
        let fragment = render(
            r#"pub fn statusline(ctx) { ctx.format("{mode}!") }"#,
            &snapshot,
        )
        .unwrap();
        assert_eq!(text(&fragment), "manual!");
        let line = render(
            r#"pub fn statusline(ctx) {
                let line = ctx.line();
                line.push(ctx.segment("mode"));
                line.push_text(" ");
                line.push_styled("hot", ctx.style("warning"));
                line
            }"#,
            &snapshot,
        )
        .unwrap();
        assert_eq!(
            line,
            [
                StyledText::new("manual", Role::Dim),
                StyledText::new(" ", Role::Normal),
                StyledText::new("hot", Role::Warning),
            ]
        );
    }

    #[test]
    fn pushes_keep_their_order() {
        let line = render(
            r#"pub fn statusline(ctx) {
                let line = ctx.line();
                line.push_text("1");
                line.push(ctx.format("2{model}3"));
                line.push_styled("4", ctx.style("bold"));
                line.push_text("5");
                line
            }"#,
            &snapshot(&[("model", "m")]),
        )
        .unwrap();
        assert_eq!(text(&line), "12m345");
    }

    #[test]
    fn a_value_that_is_not_a_line_is_an_error() {
        let error = render("pub fn statusline(ctx) { 5 }", &snapshot(&[])).unwrap_err();
        assert!(error.contains("instead of a line"), "{error}");
    }

    #[test]
    fn control_characters_never_reach_the_row() {
        let runs = render(
            r#"pub fn statusline(ctx) { "a\x1b[31mb\nc" }"#,
            &snapshot(&[]),
        )
        .unwrap();
        assert_eq!(text(&runs), "a[31mbc");
    }

    #[test]
    fn every_unknown_value_is_none_and_a_known_one_is_a_number() {
        let source = r#"pub fn statusline(ctx) {
            let none = [
                ctx.branch(), ctx.effort(), ctx.vim(), ctx.bug_drafts(), ctx.goal(),
                ctx.remote(), ctx.command_line(),
            ];
            let numbers = [
                ctx.context_used_percentage(), ctx.context_remaining_percentage(),
                ctx.context_total_tokens(), ctx.context_window_size(),
            ];
            let all_none = none.iter().all(|v| v.is_none()) && numbers.iter().all(|v| v.is_none())
                && ctx.cost_total_cost_usd().is_none();
            format!("{all_none}")
        }"#;
        let runs = render(source, &StatuslineSnapshot::default()).unwrap();
        assert_eq!(text(&runs), "true");
    }

    #[test]
    fn the_getters_return_the_snapshot() {
        let snapshot = StatuslineSnapshot {
            values: Values {
                context_used_percentage: Some(85),
                context_remaining_percentage: Some(15),
                context_total_tokens: Some(1234),
                context_window_size: Some(2000),
                cost_total_cost_usd: Some(0.25),
            },
            mode: "auto".into(),
            model: "p/m".into(),
            dir: "~/w".into(),
            branch: Some("main".into()),
            cwd: "/w".into(),
            project_dir: "/p".into(),
            session_id: "s1".into(),
            version: "1.2.3".into(),
            effort: Some("high".into()),
            vim: Some("vim:normal".into()),
            afk: true,
            afk_stopped: true,
            guardian_off: true,
            running_agents: 2,
            background_shells: 3,
            background_monitors: 4,
            scheduled: 5,
            bug_drafts: Some(6),
            goal: Some((240, false)),
            remote: Some("remote".into()),
            cost_is_estimate: true,
            command_line: Some("cmd".into()),
            ..StatuslineSnapshot::default()
        };
        let source = r#"pub fn statusline(ctx) {
            let (shells, monitors) = ctx.background_jobs();
            let pct = ctx.context_used_percentage().unwrap();
            let style = if pct >= 80 { ctx.style("warning") } else { ctx.style("dim") };
            let line = ctx.line();
            line.push_styled(
                format!(
                    "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
                    ctx.mode(), ctx.model(), ctx.dir(), ctx.branch().unwrap(), ctx.cwd(),
                    ctx.project_dir(), ctx.session_id(), ctx.version(), ctx.effort().unwrap(),
                    ctx.vim().unwrap(), ctx.afk(), ctx.afk_stopped(), ctx.guardian_off(),
                    ctx.running_agents(), shells, monitors, ctx.scheduled(),
                    ctx.bug_drafts().unwrap(), ctx.goal().unwrap(), ctx.remote().unwrap(),
                    ctx.cost_is_estimate(), ctx.command_line().unwrap(), ctx.cost_total_cost_usd().unwrap(),
                ),
                style,
            );
            line.push_text(format!(
                " {} {} {}",
                ctx.context_remaining_percentage().unwrap(),
                ctx.context_total_tokens().unwrap(),
                ctx.context_window_size().unwrap(),
            ));
            line
        }"#;
        let runs = render(source, &snapshot).unwrap();
        assert_eq!(
            text(&runs),
            "auto p/m ~/w main /w /p s1 1.2.3 high vim:normal true true true 2 3 4 5 6 4m remote true cmd 0.25 15 1234 2000"
        );
        assert_eq!(runs[0].role, Role::Warning);
    }

    #[test]
    fn a_paused_goal_reads_paused() {
        let snapshot = StatuslineSnapshot {
            goal: Some((240, true)),
            ..StatuslineSnapshot::default()
        };
        let runs = render("pub fn statusline(ctx) { ctx.goal().unwrap() }", &snapshot).unwrap();
        assert_eq!(text(&runs), "paused");
    }

    #[test]
    fn an_unknown_style_fails_at_render_time() {
        let error = render(
            r#"pub fn statusline(ctx) { ctx.line().push_styled("x", ctx.style("nope")) }"#,
            &snapshot(&[]),
        )
        .unwrap_err();
        assert!(error.contains("`nope`"), "{error}");
    }

    #[test]
    fn every_style_name_is_a_style() {
        for (style, role) in crate::core::statusline::snapshot::STYLE_NAMES {
            let source = format!(
                r#"pub fn statusline(ctx) {{
                    let line = ctx.line();
                    line.push_styled("x", ctx.style("{style}"));
                    line
                }}"#
            );
            let runs = render(&source, &StatuslineSnapshot::default()).unwrap();
            assert_eq!(runs, [StyledText::new("x", *role)], "{style}");
        }
    }

    #[test]
    fn a_syntax_error_names_the_diagnostics() {
        let error = StatuslineProgram::compile("pub fn statusline(ctx) { let = }").unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("the script does not compile: "),
            "{error}"
        );
        assert!(error.to_string().len() > "the script does not compile: ".len());
    }

    #[test]
    fn the_entry_point_is_required_with_one_parameter() {
        for source in [
            "pub fn other(ctx) { \"x\" }",
            "pub fn statusline() { \"x\" }",
            "pub fn statusline(ctx, more) { \"x\" }",
        ] {
            let error = StatuslineProgram::compile(source).unwrap_err();
            assert!(
                error.to_string().contains("pub fn statusline(ctx)"),
                "{source}: {error}"
            );
        }
        assert!(StatuslineProgram::compile("pub fn statusline(ctx) { \"x\" }").is_ok());
    }

    #[test]
    fn a_script_cannot_reach_a_name_the_host_does_not_install() {
        assert!(StatuslineProgram::compile("pub fn statusline(ctx) { println(\"x\") }").is_err());
    }

    #[test]
    fn an_unknown_template_name_fails_at_render_time_not_at_compile_time() {
        let source = r#"pub fn statusline(ctx) { ctx.format("{nope}") }"#;
        let program = StatuslineProgram::compile(source).expect("compiles");
        let error = program.render(&StatuslineSnapshot::default()).unwrap_err();
        assert!(error.contains("`nope`"), "{error}");
    }

    #[test]
    fn an_endless_loop_ends_on_the_instruction_budget() {
        let error = render(
            "pub fn statusline(ctx) { loop {} }",
            &StatuslineSnapshot::default(),
        )
        .unwrap_err();
        assert!(error.contains("failed"), "{error}");
        // The budget is restored: a normal render after it still runs.
        let ok = render(
            "pub fn statusline(ctx) { \"fine\" }",
            &StatuslineSnapshot::default(),
        );
        assert!(ok.is_ok());
    }

    #[test]
    fn a_host_panic_is_an_error_not_a_crash() {
        let error = render(
            "pub fn statusline(ctx) { host_panic(); \"x\" }",
            &StatuslineSnapshot::default(),
        )
        .unwrap_err();
        assert!(error.contains("panicked"), "{error}");
    }

    #[test]
    fn the_default_script_compiles_and_every_segment_and_value_it_names_exists() {
        assert!(StatuslineProgram::compile(DEFAULT_SCRIPT).is_ok());
        for piece in pieces(
            DEFAULT_SCRIPT
                .split('"')
                .nth(1)
                .expect("the default script holds one template"),
        ) {
            if let Piece::Name(name) = piece {
                assert!(
                    SEGMENTS.contains(&name.as_str()) || VALUES.contains(&name.as_str()),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn the_default_script_draws_each_segment_in_its_own_role_and_drops_the_empty_ones() {
        let mut snapshot = snapshot(&[("model", "m"), ("hint", "h")]);
        snapshot
            .segments
            .insert("mode", vec![StyledText::new("manual", Role::ModeManual)]);
        let runs = render(DEFAULT_SCRIPT, &snapshot).unwrap();
        assert_eq!(text(&runs), " manual · m · h");
        assert_eq!(runs[1].role, Role::ModeManual);
    }
}
