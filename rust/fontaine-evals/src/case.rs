use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use fontaine_core::config::PermissionMode;
use fontaine_core::permission_rules::parse_rule;
use fontaine_core::tier::Tier;
use fontaine_guardian::RuleLabel;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Guardian,
    Pipeline,
    Scanner,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Guardian => "guardian",
            Kind::Pipeline => "pipeline",
            Kind::Scanner => "scanner",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Full,
    Minimal,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum Category {
    Benign,
    /// The kebab-case attack class, validated against the guardian's rule labels.
    Attack(String),
    InjectionInArgs,
    AmbiguousIntent,
    Tier05Adversarial,
    Spawn,
    Floor,
    MinimalEscape,
    MinimalBenign,
}

impl TryFrom<String> for Category {
    type Error = String;

    fn try_from(value: String) -> Result<Self, String> {
        Ok(match value.as_str() {
            "benign" => Category::Benign,
            "injection-in-args" => Category::InjectionInArgs,
            "ambiguous-intent" => Category::AmbiguousIntent,
            "tier05-adversarial" => Category::Tier05Adversarial,
            "spawn" => Category::Spawn,
            "floor" => Category::Floor,
            "minimal-escape" => Category::MinimalEscape,
            "minimal-benign" => Category::MinimalBenign,
            other => match other.strip_prefix("attack:") {
                Some(class) if RuleLabel::from_attack_class(class).is_some() => {
                    Category::Attack(class.to_string())
                }
                Some(class) => {
                    return Err(format!(
                        "unknown attack class `{class}`; expected one of: {}",
                        RuleLabel::attack_classes().collect::<Vec<_>>().join(", ")
                    ));
                }
                None => return Err(format!("unknown category `{other}`")),
            },
        })
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Category::Benign => f.write_str("benign"),
            Category::Attack(class) => write!(f, "attack:{class}"),
            Category::InjectionInArgs => f.write_str("injection-in-args"),
            Category::AmbiguousIntent => f.write_str("ambiguous-intent"),
            Category::Tier05Adversarial => f.write_str("tier05-adversarial"),
            Category::Spawn => f.write_str("spawn"),
            Category::Floor => f.write_str("floor"),
            Category::MinimalEscape => f.write_str("minimal-escape"),
            Category::MinimalBenign => f.write_str("minimal-benign"),
        }
    }
}

/// The canned outcome fixture mode returns: a plain label, or `{decision, tier}` for
/// pipeline cases.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum FixtureVerdict {
    Pipeline(PipelineVerdict),
    Label(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineVerdict {
    pub decision: String,
    pub tier: Tier,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub symlinks: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub allow_extra: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoModeOverrides {
    pub environment: Option<Vec<String>>,
    pub allow: Option<Vec<String>>,
    pub soft_deny: Option<Vec<String>>,
    pub hard_deny: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub kind: Kind,
    pub category: Category,
    pub profile: Profile,
    pub task_intent: String,
    pub tool: String,
    pub args: serde_json::Value,
    pub tool_result: Option<String>,
    pub expected: String,
    pub expected_tier: Option<Tier>,
    pub fixture_verdict: FixtureVerdict,
    pub recent_user_messages: Option<Vec<String>>,
    pub session_facts: Option<serde_json::Value>,
    pub mode: Option<PermissionMode>,
    pub workspace: Option<Workspace>,
    pub auto_mode: Option<AutoModeOverrides>,
    pub environment: Option<Vec<String>>,
    pub permissions: Option<Permissions>,
}

impl Case {
    /// The decision a pipeline case's mode name resolves to; `manual` when unset.
    pub fn mode(&self) -> PermissionMode {
        self.mode.unwrap_or(PermissionMode::Manual)
    }
}

const GUARDIAN_VERDICTS: [&str; 3] = ["allow", "deny", "escalate"];
const PIPELINE_DECISIONS: [&str; 3] = ["allow", "deny", "prompt"];
const SCANNER_VERDICTS: [&str; 2] = ["flagged", "clean"];

/// Everything a case must satisfy beyond deserializing; returns one message per problem.
pub fn validate_case(case: &Case) -> Vec<String> {
    let mut errors = Vec::new();
    let mut err = |msg: String| errors.push(msg);

    if case.id.trim().is_empty() {
        err("`id` must not be empty".into());
    }
    if !case.args.is_object() {
        err("`args` must be a JSON object".into());
    }
    if case.tool.trim().is_empty() {
        err("`tool` must not be empty".into());
    }

    let (allowed_labels, allowed_categories): (&[&str], fn(&Category) -> bool) = match case.kind {
        Kind::Guardian => (&GUARDIAN_VERDICTS, |c| {
            !matches!(
                c,
                Category::Tier05Adversarial | Category::Spawn | Category::Floor
            )
        }),
        Kind::Pipeline => (&PIPELINE_DECISIONS, |c| {
            matches!(
                c,
                Category::Tier05Adversarial | Category::Spawn | Category::Floor
            )
        }),
        Kind::Scanner => (&SCANNER_VERDICTS, |c| {
            matches!(
                c,
                Category::Spawn | Category::InjectionInArgs | Category::Benign
            )
        }),
    };
    if !allowed_categories(&case.category) {
        err(format!(
            "category `{}` is not valid for kind `{}`",
            case.category,
            case.kind.as_str()
        ));
    }
    if !allowed_labels.contains(&case.expected.as_str()) {
        err(format!(
            "`expected` for kind `{}` must be one of: {}",
            case.kind.as_str(),
            allowed_labels.join(", ")
        ));
    }

    match (&case.fixture_verdict, case.kind) {
        (FixtureVerdict::Pipeline(_), Kind::Pipeline) => {}
        (FixtureVerdict::Label(label), Kind::Guardian | Kind::Scanner) => {
            if !allowed_labels.contains(&label.as_str()) {
                err(format!(
                    "`fixture_verdict` for kind `{}` must be one of: {}",
                    case.kind.as_str(),
                    allowed_labels.join(", ")
                ));
            }
        }
        (_, Kind::Pipeline) => err("pipeline `fixture_verdict` must be `{decision, tier}`".into()),
        (_, _) => err(format!(
            "`fixture_verdict` for kind `{}` must be a plain label",
            case.kind.as_str()
        )),
    }
    if let FixtureVerdict::Pipeline(v) = &case.fixture_verdict
        && !PIPELINE_DECISIONS.contains(&v.decision.as_str())
    {
        err(format!(
            "`fixture_verdict.decision` must be one of: {}",
            PIPELINE_DECISIONS.join(", ")
        ));
    }

    if matches!(case.category, Category::Attack(_) | Category::MinimalEscape)
        && case.expected != "deny"
    {
        err("attack and escape cases expect `deny`".into());
    }
    if matches!(
        case.category,
        Category::MinimalEscape | Category::MinimalBenign
    ) && case.profile != Profile::Minimal
    {
        err("`minimal-*` categories require `profile = \"minimal\"`".into());
    }

    match case.kind {
        Kind::Pipeline => {
            if case.expected_tier.is_none() {
                err("pipeline cases require `expected_tier`".into());
            }
        }
        _ => {
            if case.expected_tier.is_some() {
                err("`expected_tier` applies to pipeline cases only".into());
            }
            if case.permissions.is_some() || case.mode.is_some() {
                err("`mode` and `permissions` apply to pipeline cases only".into());
            }
        }
    }
    match (case.kind, &case.tool_result) {
        (Kind::Scanner, None) => err("scanner cases require `tool_result`".into()),
        (Kind::Scanner, Some(_)) | (_, None) => {}
        (_, Some(_)) => err("`tool_result` applies to scanner cases only".into()),
    }

    if let Some(p) = &case.permissions {
        for (list, rules) in [("deny", &p.deny), ("ask", &p.ask), ("allow", &p.allow)] {
            for rule in rules {
                if let Err(e) = parse_rule(rule) {
                    err(format!("permissions.{list}: {e}"));
                }
            }
        }
    }
    if let Some(ws) = &case.workspace {
        for path in ws.files.keys().chain(ws.symlinks.keys()) {
            if path.starts_with('/') || path.split('/').any(|c| c == "..") {
                err(format!(
                    "workspace path `{path}` must be relative and inside the root"
                ));
            }
        }
    }
    errors
}

/// Loads and validates every `*.jsonl` file in `dir`, in file-name order. On failure returns
/// every problem found, each prefixed `file:line`.
pub fn load_dataset(dir: &Path) -> Result<Vec<Case>, Vec<String>> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        vec![format!(
            "cannot read dataset directory {}: {e}",
            dir.display()
        )]
    })?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl") && p.is_file())
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(vec![format!("no *.jsonl files in {}", dir.display())]);
    }

    let mut cases = Vec::new();
    let mut errors = Vec::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for file in files {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) => {
                errors.push(format!("{name}: cannot read: {e}"));
                continue;
            }
        };
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let at = format!("{name}:{}", i + 1);
            let case: Case = match serde_json::from_str(line) {
                Ok(case) => case,
                Err(e) => {
                    errors.push(format!("{at}: {e}"));
                    continue;
                }
            };
            let problems = validate_case(&case);
            let label = format!("{at} ({})", case.id);
            errors.extend(problems.into_iter().map(|p| format!("{label}: {p}")));
            if let Some(first) = seen.insert(case.id.clone(), at.clone()) {
                errors.push(format!("{label}: duplicate id, first seen at {first}"));
            }
            cases.push(case);
        }
    }
    if errors.is_empty() {
        Ok(cases)
    } else {
        Err(errors)
    }
}

pub const MIN_PER_ATTACK_CLASS: usize = 10;
pub const MIN_BENIGN: usize = 30;
pub const MIN_AMBIGUOUS: usize = 15;
pub const MIN_TIER05: usize = 20;

/// Reports every category below the seed dataset's minimum case count.
pub fn minimum_count_shortfalls(cases: &[Case]) -> Vec<String> {
    let count = |pred: &dyn Fn(&Case) -> bool| cases.iter().filter(|c| pred(c)).count();
    let mut out = Vec::new();
    let mut check = |name: String, have: usize, need: usize| {
        if have < need {
            out.push(format!("{name}: {have} cases, need at least {need}"));
        }
    };
    for class in RuleLabel::attack_classes() {
        let want = Category::Attack(class.to_string());
        check(
            format!("attack:{class}"),
            count(&|c| c.kind == Kind::Guardian && c.category == want),
            MIN_PER_ATTACK_CLASS,
        );
    }
    check(
        "benign".into(),
        count(&|c| c.kind == Kind::Guardian && c.category == Category::Benign),
        MIN_BENIGN,
    );
    check(
        "ambiguous-intent".into(),
        count(&|c| c.category == Category::AmbiguousIntent),
        MIN_AMBIGUOUS,
    );
    check(
        "tier05-adversarial".into(),
        count(&|c| c.category == Category::Tier05Adversarial),
        MIN_TIER05,
    );
    out
}
