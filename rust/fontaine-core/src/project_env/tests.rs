use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use fontaine_guardian::env_scan::{Evidence, Finding, Reply};
use fontaine_guardian::{EnvFileFacts, Protection, Visibility};
use futures::future::BoxFuture;

use super::discover::{Gh, GhOutcome, github_slug, host_of, is_env_like, parse_workflow};
use super::sample::{self, Sample};
use super::*;
use crate::config::{AutoMode, PermissionMode};

struct FakeGh {
    answers: HashMap<String, GhOutcome>,
    calls: Mutex<Vec<String>>,
}

impl FakeGh {
    fn new(answers: &[(&str, GhOutcome)]) -> FakeGh {
        FakeGh {
            answers: answers
                .iter()
                .map(|(args, outcome)| ((*args).to_string(), outcome.clone()))
                .collect(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Gh for FakeGh {
    fn run<'a>(&'a self, args: &'a [&'a str]) -> BoxFuture<'a, GhOutcome> {
        let key = args.join(" ");
        self.calls.lock().unwrap().push(key.clone());
        let outcome = self
            .answers
            .get(&key)
            .cloned()
            .unwrap_or_else(|| GhOutcome::Failed("HTTP 404: Not Found".into()));
        Box::pin(async move { outcome })
    }
}

fn ok(text: &str) -> GhOutcome {
    GhOutcome::Output(text.to_string())
}

const URL: &str = "https://github.com/acme/proj.git";

fn view(visibility: &str) -> GhOutcome {
    ok(&format!(
        r#"{{"visibility":"{visibility}","defaultBranchRef":{{"name":"main"}}}}"#
    ))
}

const VIEW: &str = "repo view acme/proj --json visibility,defaultBranchRef";
const BRANCH: &str = "api repos/acme/proj/branches/main";
const RULES: &str = "api repos/acme/proj/rules/branches/main";
const ENVS: &str = "api repos/acme/proj/environments";
const SETTINGS: &str = "repo view acme/proj --json homepageUrl,isFork,parent,repositoryTopics";
const PAGES: &str = "api repos/acme/proj/pages";

async fn discovered(gh: &FakeGh, root: &Path, remotes: &[&str]) -> discover::Discovery {
    let remotes: Vec<String> = remotes.iter().map(|r| (*r).to_string()).collect();
    discover::discover(gh, root, &remotes, "2026-10-30T00:00:00.000Z".into()).await
}

use std::path::Path;

fn one_remote(found: &discover::Discovery) -> &RemoteFacts {
    assert_eq!(found.record.remotes.len(), 1);
    &found.record.remotes[0]
}

#[tokio::test]
async fn a_public_repository_with_a_protected_branch_and_environments() {
    let dir = tempfile::tempdir().unwrap();
    let gh = FakeGh::new(&[
        (VIEW, view("PUBLIC")),
        (BRANCH, ok(r#"{"protected":true}"#)),
        (
            RULES,
            ok(r#"[{"type":"pull_request"},{"type":"non_fast_forward"},{"type":"pull_request"}]"#),
        ),
        (
            ENVS,
            ok(r#"{"environments":[{"name":"pages"},{"name":"github-pages"}]}"#),
        ),
        (
            SETTINGS,
            ok(
                r#"{"homepageUrl":"https://docs.acme.dev/x","isFork":true,"parent":{"name":"up","owner":{"login":"o"}},"repositoryTopics":[{"name":"rust"}]}"#,
            ),
        ),
        (
            PAGES,
            ok(r#"{"html_url":"https://acme.github.io/proj/","cname":"docs.acme.dev"}"#),
        ),
    ]);
    let found = discovered(&gh, dir.path(), &[URL]).await;
    let remote = one_remote(&found);
    assert_eq!(remote.slug.as_deref(), Some("acme/proj"));
    assert_eq!(remote.visibility, Visibility::Public);
    assert_eq!(remote.default_branch.as_deref(), Some("main"));
    assert_eq!(remote.default_branch_protection, Protection::Protected);
    assert_eq!(remote.branch_rules, ["non_fast_forward", "pull_request"]);
    assert_eq!(remote.deploy_environments, ["github-pages", "pages"]);
    assert_eq!(found.settings["github.visibility"], "public");
    assert_eq!(found.settings["github.pages.cname"], "docs.acme.dev");
    assert_eq!(found.settings["github.pages.url"], "acme.github.io");
    assert_eq!(found.settings["github.homepage"], "docs.acme.dev");
    assert_eq!(found.settings["github.fork_of"], "o/up");
    assert_eq!(found.settings["github.topics"], "rust");
    assert!(found.fallbacks.is_empty(), "{:?}", found.fallbacks);
}

#[tokio::test]
async fn private_and_internal_map_and_no_pages_site_is_not_a_fallback() {
    for (given, want) in [
        ("PRIVATE", Visibility::Private),
        ("INTERNAL", Visibility::Internal),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let gh = FakeGh::new(&[
            (VIEW, view(given)),
            (BRANCH, ok(r#"{"protected":false}"#)),
            (RULES, ok("[]")),
            (ENVS, ok(r#"{"environments":[]}"#)),
            (SETTINGS, ok(r#"{"homepageUrl":"","isFork":false}"#)),
        ]);
        let found = discovered(&gh, dir.path(), &[URL]).await;
        let remote = one_remote(&found);
        assert_eq!(remote.visibility, want);
        assert_eq!(remote.default_branch_protection, Protection::Unprotected);
        assert!(!found.settings.contains_key("github.pages.cname"));
        assert!(found.fallbacks.is_empty(), "{:?}", found.fallbacks);
    }
}

#[tokio::test]
async fn rules_alone_make_a_branch_protected() {
    let dir = tempfile::tempdir().unwrap();
    let gh = FakeGh::new(&[
        (VIEW, view("PUBLIC")),
        (BRANCH, ok(r#"{"protected":false}"#)),
        (RULES, ok(r#"[{"type":"required_status_checks"}]"#)),
    ]);
    let found = discovered(&gh, dir.path(), &[URL]).await;
    let remote = one_remote(&found);
    assert_eq!(remote.default_branch_protection, Protection::Protected);
    assert_eq!(remote.branch_rules, ["required_status_checks"]);
}

#[tokio::test]
async fn a_refused_check_is_unverified_and_a_missing_gh_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let refused = FakeGh::new(&[(
        VIEW,
        GhOutcome::Failed(
            "GraphQL: Resource protected by organization SAML enforcement (HTTP 403)".into(),
        ),
    )]);
    let found = discovered(&refused, dir.path(), &[URL]).await;
    let remote = one_remote(&found);
    assert_eq!(remote.visibility, Visibility::Unverified);
    assert_eq!(remote.default_branch_protection, Protection::Unknown);
    assert!(
        found.fallbacks[0].contains("visibility"),
        "{:?}",
        found.fallbacks
    );

    for outcome in [
        GhOutcome::Missing,
        GhOutcome::TimedOut,
        GhOutcome::Failed("To get started with GitHub CLI, please run:  gh auth login".into()),
    ] {
        let gh = FakeGh::new(&[(VIEW, outcome)]);
        let found = discovered(&gh, dir.path(), &[URL]).await;
        assert_eq!(one_remote(&found).visibility, Visibility::Unknown);
        assert!(!found.fallbacks.is_empty());
    }
    let missing = FakeGh::new(&[(VIEW, GhOutcome::Missing)]);
    let found = discovered(&missing, dir.path(), &[URL]).await;
    assert!(found.fallbacks[0].contains("`gh` is not installed"));
}

#[tokio::test]
async fn other_hosts_are_unknown_and_never_reach_gh() {
    let dir = tempfile::tempdir().unwrap();
    let gh = FakeGh::new(&[]);
    let found = discovered(
        &gh,
        dir.path(),
        &[
            "https://git.example.com/acme/proj.git",
            "git@gitlab.com:acme/proj.git",
        ],
    )
    .await;
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());
    assert_eq!(found.record.remotes.len(), 2);
    assert!(
        found
            .record
            .remotes
            .iter()
            .all(|r| r.visibility == Visibility::Unknown && r.slug.is_none())
    );
}

#[test]
fn github_slugs_come_from_every_url_form_and_nothing_else() {
    for url in [
        "https://github.com/acme/proj.git",
        "https://github.com/acme/proj",
        "git@github.com:acme/proj.git",
        "ssh://git@github.com/acme/proj.git",
        "https://GitHub.com/acme/proj/",
    ] {
        assert_eq!(github_slug(url).as_deref(), Some("acme/proj"), "{url}");
    }
    for url in [
        "https://github.com.evil.example/acme/proj",
        "https://gitlab.com/acme/proj",
        "https://github.com/-acme/proj",
        "https://github.com/acme/pr oj",
        "https://github.com/acme",
    ] {
        assert_eq!(github_slug(url), None, "{url}");
    }
}

#[test]
fn workflow_triggers_parse_as_string_list_or_map() {
    let release = |text: &str| parse_workflow(text).tag_release;
    assert!(release("on: release\njobs: {}\n"));
    assert!(release("on: [push, create]\njobs: {}\n"));
    assert!(!release("on: [push, pull_request]\njobs: {}\n"));
    assert!(release("on:\n  push:\n    tags: ['v*']\n"));
    assert!(release("on:\n  release:\n    types: [published]\n"));
    assert!(!release("on:\n  push:\n    branches: [main]\n"));
    assert!(!release("on: push\n"));
    assert!(release("true:\n  push:\n    tags: v*\n"));
}

#[test]
fn job_environments_are_a_string_or_a_name_map_and_bad_yaml_is_empty() {
    let parsed = parse_workflow(
        "on: push\njobs:\n  a:\n    environment: pages\n  b:\n    environment:\n      name: prod\n      url: https://x\n  c:\n    runs-on: x\n",
    );
    assert_eq!(parsed.environments, ["pages", "prod"]);
    assert_eq!(
        parse_workflow("on: [unclosed\n  : : :\n\t- ["),
        discover::ParsedWorkflow::default()
    );
    assert_eq!(parse_workflow(""), discover::ParsedWorkflow::default());
}

#[tokio::test]
async fn workflows_are_read_from_the_git_root_not_the_primary_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
    std::fs::create_dir_all(root.join("rust")).unwrap();
    gix::init(&root).unwrap();
    std::fs::write(
        root.join(".github/workflows/release.yml"),
        "on:\n  push:\n    tags: ['v*']\njobs:\n  p:\n    environment: pages\n",
    )
    .unwrap();
    std::fs::write(root.join(".github/workflows/ci.yml"), "on: push\n").unwrap();
    std::fs::write(root.join("rust/Cargo.toml"), "[package]\n").unwrap();
    let gh = FakeGh::new(&[]);
    let found = discovered(&gh, &root.join("rust"), &[]).await;
    assert_eq!(
        found.record.tag_release_workflows,
        [".github/workflows/release.yml"]
    );
    assert_eq!(found.record.workflow_environments, ["pages"]);
    assert_eq!(found.record.stack, ["rust"]);
    assert_eq!(
        found.repo_root.canonicalize().unwrap(),
        root.canonicalize().unwrap()
    );
}

#[tokio::test]
async fn registries_keep_the_host_only_and_no_secret_survives() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join(".npmrc"),
        "registry=https://me:npm_SECRETTOKEN123@npm.acme.internal/path?token=SECRETQUERY\n@acme:registry=https://scoped.acme.internal/\n//npm.acme.internal/:_authToken=SECRETAUTH\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".yarnrc.yml"),
        "npmRegistryServer: \"https://yarn.acme.internal\"\nnpmAuthToken: SECRETYARN\nnpmScopes:\n  s:\n    npmRegistryServer: https://scope.yarn.internal\n",
    )
    .unwrap();
    std::fs::write(
        root.join("pip.conf"),
        "[global]\nindex-url = https://u:SECRETPIP@pip.acme.internal/simple\nextra-index-url =\n    https://extra.acme.internal/simple\n",
    )
    .unwrap();
    std::fs::write(
        root.join("pyproject.toml"),
        "[[tool.uv.index]]\nname = \"x\"\nurl = \"https://uv.acme.internal/simple\"\n[[tool.poetry.source]]\nname = \"p\"\nurl = \"https://poetry.acme.internal/simple\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".cargo")).unwrap();
    std::fs::write(
        root.join(".cargo/config.toml"),
        "[registries.corp]\nindex = \"sparse+https://cargo.acme.internal/index/\"\ntoken = \"SECRETCARGO\"\n[source.mirror]\nregistry = \"https://mirror.acme.internal/\"\n",
    )
    .unwrap();
    let gh = FakeGh::new(&[]);
    let found = discovered(&gh, root, &[]).await;
    let hosts: HashSet<&str> = found
        .record
        .package_registries
        .iter()
        .map(|r| r.host.as_str())
        .collect();
    for host in [
        "npm.acme.internal",
        "scoped.acme.internal",
        "yarn.acme.internal",
        "scope.yarn.internal",
        "pip.acme.internal",
        "extra.acme.internal",
        "uv.acme.internal",
        "poetry.acme.internal",
        "cargo.acme.internal",
        "mirror.acme.internal",
    ] {
        assert!(hosts.contains(host), "{host} in {hosts:?}");
    }
    let dump = format!(
        "{}{:?}",
        serde_json::to_string(&found.record).unwrap(),
        found.settings
    );
    for secret in [
        "SECRET", "npm_", "me:npm", "token", "/path", "/simple", "/index",
    ] {
        assert!(!dump.contains(secret), "{secret} leaked: {dump}");
    }
    assert_eq!(
        found.settings["config:.npmrc:registry"],
        "npm.acme.internal"
    );
}

#[test]
fn env_like_names() {
    for name in [".env", ".env.local", "prod.env", "cfg/.env.production"] {
        assert!(is_env_like(name), "{name}");
    }
    for name in [".envrc", "environment.rs", "env.rs"] {
        assert!(!is_env_like(name), "{name}");
    }
}

fn git_repo(root: &Path) {
    gix::init(root).unwrap();
}

#[tokio::test]
async fn env_files_report_ignored_and_tracked_ones_without_reading_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_repo(root);
    std::fs::write(root.join(".gitignore"), ".env\nsub/.env.local\n").unwrap();
    std::fs::write(root.join(".env"), "TOKEN=SECRETVALUE\n").unwrap();
    std::fs::write(root.join(".env.example"), "TOKEN=\n").unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/.env.local"), "TOKEN=SECRETVALUE\n").unwrap();
    let gh = FakeGh::new(&[]);
    let found = discovered(&gh, root, &[]).await;
    assert_eq!(
        found.record.env_files,
        [
            EnvFileFacts {
                name: ".env".into(),
                gitignored: true
            },
            EnvFileFacts {
                name: ".env.example".into(),
                gitignored: false
            },
            EnvFileFacts {
                name: "sub/.env.local".into(),
                gitignored: true
            },
        ]
    );
    assert!(
        !serde_json::to_string(&found.record)
            .unwrap()
            .contains("SECRETVALUE")
    );
}

fn gathered(root: &Path, settings: &[(&str, &str)]) -> Sample {
    let settings: BTreeMap<String, String> = settings
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    sample::gather(root, &settings)
}

fn track(root: &Path, paths: &[&str]) {
    let repo = gix::open(root).unwrap();
    let mut index = gix::index::File::at_or_default(
        repo.index_path(),
        repo.object_hash(),
        false,
        Default::default(),
    )
    .unwrap();
    for path in paths {
        let id = repo
            .write_blob(std::fs::read(root.join(path)).unwrap())
            .unwrap();
        index.dangerously_push_entry(
            Default::default(),
            id.detach(),
            gix::index::entry::Flags::empty(),
            gix::index::entry::Mode::FILE,
            gix::bstr::BStr::new(path.as_bytes()),
        );
    }
    index.sort_entries();
    index.write(Default::default()).unwrap();
}

#[test]
fn the_sample_holds_caps_and_never_the_contents_of_env_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_repo(root);
    std::fs::write(root.join(".env"), "API_KEY=PLANTEDSECRETVALUE\n").unwrap();
    std::fs::write(root.join("Makefile"), "deploy:\n\t./ship.sh\n").unwrap();
    std::fs::write(root.join("README.md"), "x".repeat(50_000)).unwrap();
    std::fs::write(
        root.join("package.json"),
        r#"{"name":"n","secret":"PLANTEDSECRETPKG","scripts":{"deploy":"fly deploy"}}"#,
    )
    .unwrap();
    std::fs::write(root.join("prod.env"), "K=PLANTEDSECRETVALUE\n").unwrap();
    let mut paths = vec![".env", "Makefile", "README.md", "package.json", "prod.env"];
    for i in 0..2_500 {
        std::fs::write(root.join(format!("f{i:04}.txt")), "").unwrap();
    }
    let names: Vec<String> = (0..2_500).map(|i| format!("f{i:04}.txt")).collect();
    paths.extend(names.iter().map(String::as_str));
    track(root, &paths);

    let sample = gathered(root, &[("github.visibility", "public")]);
    assert!(sample.text.chars().count() <= sample::SAMPLE_CAP + 100);
    assert!(sample.listing.len() <= sample::LISTING_PATHS);
    assert!(sample.listing.contains(".env"), "named in the listing");
    assert!(
        !sample.text.contains("PLANTEDSECRET"),
        "no secret value is sent"
    );
    assert!(sample.text.contains("fly deploy"));
    assert!(sample.text.contains("deploy:"));
    assert!(sample.text.contains("github.visibility: public"));
    let readme = sample.text.split("file README.md:").nth(1).unwrap();
    assert!(
        readme.chars().count() < sample::FILE_CAP + 100,
        "per-file cap"
    );
    assert!(sample.tracked.len() > sample::LISTING_PATHS);
}

fn finding(value: &str, evidence: Evidence) -> Finding {
    Finding {
        value: value.into(),
        evidence,
    }
}

fn file(value: &str, path: &str) -> Finding {
    finding(value, Evidence::File(path.into()))
}

fn setting(value: &str, key: &str) -> Finding {
    finding(value, Evidence::Setting(key.into()))
}

fn sample_for_validation() -> Sample {
    let tracked: Vec<String> = ["config/secrets.yml", "Makefile", "src/lib.rs"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    Sample {
        text: String::new(),
        listing: tracked.iter().cloned().collect(),
        tracked,
        settings: BTreeMap::from([(
            "github.pages.cname".to_string(),
            "docs.acme.dev".to_string(),
        )]),
    }
}

fn dropped(reply: Reply) -> (ScanFacts, Vec<String>) {
    let validated = sample::validate(reply, &sample_for_validation(), "a/m");
    (validated.scan, validated.dropped)
}

#[test]
fn a_valid_reply_survives_and_unsupported_evidence_is_dropped_and_counted() {
    let (scan, dropped) = dropped(Reply {
        sensitive_locations: vec![
            file("config/secrets.yml", "config/secrets.yml"),
            file("config/missing.yml", "config/secrets.yml"),
            file("src/*.rs", "src/not-listed.rs"),
            file("config/*.yml", "config/secrets.yml"),
        ],
        deploy_targets: vec![file("make deploy", "Makefile")],
        protected_targets: vec![setting("prod", "github.nothing")],
        trusted_destinations: vec![
            setting("docs.acme.dev", "github.pages.cname"),
            setting("docs.acme.dev", "github.pages.cname"),
        ],
    });
    assert_eq!(scan.model, "a/m");
    let values = |list: &[Finding]| list.iter().map(|f| f.value.clone()).collect::<Vec<_>>();
    assert_eq!(
        values(&scan.sensitive_locations),
        ["config/secrets.yml", "config/*.yml"]
    );
    assert_eq!(values(&scan.deploy_targets), ["make deploy"]);
    assert!(scan.protected_targets.is_empty());
    assert_eq!(values(&scan.trusted_destinations), ["docs.acme.dev"]);
    assert_eq!(dropped.len(), 4, "{dropped:?}");
    assert!(
        dropped
            .iter()
            .any(|d| d.contains("matches no tracked path"))
    );
    assert!(dropped.iter().any(|d| d.contains("does not exist")));
    assert!(
        dropped
            .iter()
            .any(|d| d.contains("not in the tracked-path listing"))
    );
}

#[test]
fn over_cap_multi_line_and_control_values_are_dropped_and_lists_are_capped() {
    let long = "a".repeat(201);
    let mut many: Vec<Finding> = (0..35)
        .map(|i| file(&format!("t{i}"), "Makefile"))
        .collect();
    many.push(file(&long, "Makefile"));
    many.push(file("two\nlines", "Makefile"));
    many.push(file("esc\u{1b}[31m", "Makefile"));
    many.push(file("  ", "Makefile"));
    let (scan, dropped) = dropped(Reply {
        deploy_targets: many,
        ..Reply::default()
    });
    assert_eq!(scan.deploy_targets.len(), sample::LIST_CAP);
    assert_eq!(dropped.len(), 5 + 4, "{dropped:?}");
}

#[test]
fn trusted_destinations_must_be_hosts_buckets_or_registry_urls() {
    for good in [
        "npm.acme.internal",
        "https://npm.acme.internal/registry",
        "s3://acme-artifacts",
        "gs://acme-prod",
        "docs.acme.dev:8443",
        "acme-*.s3.amazonaws.com",
        "s3://acme-*",
        "sparse+https://cargo.acme.internal/index/",
    ] {
        assert!(sample::is_destination(good), "{good}");
    }
    for bad in [
        "*",
        "*.com",
        "*.acme.dev",
        "*.*",
        "ac*.s3.amazonaws.com",
        "acme.*",
        "https://*",
        "localhost",
        "just words",
        "https://user:pw@host.example",
        "ftp://host.example",
        "host.example?x=1",
        "host.example/path",
        "https://host.example/*",
        "https://",
        "",
    ] {
        assert!(!sample::is_destination(bad), "{bad}");
    }
}

#[test]
fn an_extra_key_in_the_reply_is_ignored() {
    let reply = fontaine_guardian::env_scan::parse(
        r#"{"allow":["rm -rf /"],"visibility":"public","deploy_targets":[{"value":"make deploy","evidence":{"file":"Makefile"}}]}"#,
    )
    .unwrap();
    let (scan, dropped) = dropped(reply);
    assert_eq!(scan.deploy_targets.len(), 1);
    assert!(dropped.is_empty());
}

#[test]
fn the_review_lists_relaxing_entries_first() {
    let (scan, _) = dropped(Reply {
        sensitive_locations: vec![file("config/secrets.yml", "config/secrets.yml")],
        trusted_destinations: vec![setting("docs.acme.dev", "github.pages.cname")],
        ..Reply::default()
    });
    let text = sample::review_text(&scan);
    let relax = text.find("RELAX").unwrap();
    let tighten = text.find("Sensitive locations").unwrap();
    assert!(relax < tighten, "{text}");
    assert!(text.contains("evidence: setting github.pages.cname"));
    assert!(text.contains("evidence: file config/secrets.yml"));
}

fn record(discovered_at: &str, visibility: Visibility) -> ProjectEnvironment {
    ProjectEnvironment {
        version: VERSION,
        discovered_at: discovered_at.into(),
        remotes: vec![RemoteFacts {
            url: URL.into(),
            slug: Some("acme/proj".into()),
            visibility,
            default_branch: Some("main".into()),
            default_branch_protection: Protection::Protected,
            branch_rules: vec!["pull_request".into()],
            deploy_environments: Vec::new(),
        }],
        tag_release_workflows: vec![".github/workflows/release.yml".into()],
        workflow_environments: Vec::new(),
        package_registries: Vec::new(),
        env_files: Vec::new(),
        stack: vec!["rust".into()],
        scan: None,
    }
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn store_and_load_round_trip_owner_only() {
    let data = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let saved = record("2026-10-30T00:00:00.000Z", Visibility::Public);
    let file = store(data.path(), work.path(), &saved).unwrap();
    assert!(file.starts_with(data.path().join("auto_mode")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir = std::fs::metadata(file.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir, 0o700);
    }
    let loaded = load(data.path(), work.path());
    assert_eq!(loaded.record, Some(saved));
    assert_eq!(loaded.warning, None);
}

#[test]
fn a_missing_newer_or_corrupt_file_is_none_and_the_last_two_warn() {
    let data = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    assert_eq!(load(data.path(), work.path()), Loaded::default());

    let file = store(
        data.path(),
        work.path(),
        &record("2026-10-30T00:00:00.000Z", Visibility::Public),
    )
    .unwrap();
    let mut newer: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    newer["version"] = serde_json::json!(VERSION + 1);
    std::fs::write(&file, newer.to_string()).unwrap();
    let loaded = load(data.path(), work.path());
    assert!(loaded.record.is_none());
    assert!(loaded.warning.unwrap().contains("newer than this build"));

    std::fs::write(&file, "{ not json").unwrap();
    let loaded = load(data.path(), work.path());
    assert!(loaded.record.is_none());
    assert!(
        loaded
            .warning
            .unwrap()
            .contains("ignoring the project environment")
    );
}

#[test]
fn filter_to_remotes_drops_the_facts_of_remotes_that_went() {
    let mut saved = record("2026-10-30T00:00:00.000Z", Visibility::Public);
    let mut gone = saved.remotes[0].clone();
    gone.url = "https://github.com/acme/old.git".into();
    saved.remotes.push(gone);
    let kept = filter_to_remotes(saved, &[URL.to_string()]);
    assert_eq!(kept.remotes.len(), 1);
    assert_eq!(kept.remotes[0].url, URL);
}

#[test]
fn public_expires_after_thirty_days_and_nothing_else_does() {
    let saved = record("2026-10-01T00:00:00.000Z", Visibility::Public);
    let young = expire(saved.clone(), at("2026-10-30T00:00:00Z"));
    assert_eq!(young.remotes[0].visibility, Visibility::Public);
    let old = expire(saved, at("2026-11-01T00:00:00Z"));
    assert_eq!(
        old.remotes[0].visibility,
        Visibility::Expired("2026-10-01".into())
    );
    let facts = to_facts(&old);
    assert_eq!(
        facts.remotes[0].visibility,
        Visibility::Expired("2026-10-01".into())
    );
    let private = record("2020-01-01T00:00:00.000Z", Visibility::Private);
    let kept = expire(private.clone(), at("2026-11-01T00:00:00Z"));
    assert_eq!(kept, private);
}

#[test]
fn an_unparseable_date_counts_as_expired() {
    let saved = record("yesterday", Visibility::Public);
    let old = expire(saved, at("2026-10-30T00:00:00Z"));
    assert_eq!(
        old.remotes[0].visibility,
        Visibility::Expired("yesterday".into())
    );
}

fn notice(
    saved: Option<&ProjectEnvironment>,
    remotes: &[&str],
    mode: PermissionMode,
    reminder: bool,
    now: &str,
) -> Option<crate::notice::Notice> {
    let auto_mode = AutoMode {
        setup_reminder: reminder,
        ..AutoMode::default()
    };
    let remotes: Vec<String> = remotes.iter().map(|r| (*r).to_string()).collect();
    setup_notice(saved, &remotes, mode, &auto_mode, at(now))
}

#[test]
fn the_setup_notice_matrix() {
    let fresh = record("2026-10-20T00:00:00.000Z", Visibility::Public);
    let now = "2026-10-30T00:00:00Z";
    // Missing.
    let missing = notice(None, &[URL], PermissionMode::Auto, true, now).unwrap();
    assert!(
        missing
            .headline
            .contains("doesn't know this project's environment")
    );
    assert!(missing.hint.unwrap().contains("fontaine auto-mode setup"));
    assert!(notice(None, &[URL], PermissionMode::Manual, true, now).is_none());
    assert!(notice(None, &[URL], PermissionMode::Auto, false, now).is_none());
    // Fresh.
    assert!(notice(Some(&fresh), &[URL], PermissionMode::Auto, true, now).is_none());
    // A remote with no entry.
    let other = "https://github.com/acme/other.git";
    let stale = notice(Some(&fresh), &[URL, other], PermissionMode::Auto, true, now).unwrap();
    assert!(stale.headline.contains(other));
    assert!(
        stale
            .hint
            .unwrap()
            .contains("rerun `fontaine auto-mode setup`")
    );
    // An old public visibility.
    let old = record("2026-08-01T00:00:00.000Z", Visibility::Public);
    let stale = notice(Some(&old), &[URL], PermissionMode::Auto, true, now).unwrap();
    assert!(stale.headline.contains("public"));
    assert!(notice(Some(&old), &[URL], PermissionMode::Manual, true, now).is_none());
    assert!(notice(Some(&old), &[URL], PermissionMode::Auto, false, now).is_none());
    // An old private one is fine.
    let private = record("2026-08-01T00:00:00.000Z", Visibility::Private);
    assert!(notice(Some(&private), &[URL], PermissionMode::Auto, true, now).is_none());
}

#[test]
fn host_of_strips_everything_but_the_host() {
    assert_eq!(
        host_of("https://u:p@Host.Example:8443/a/b?q=1#f").as_deref(),
        Some("host.example")
    );
    assert_eq!(
        host_of("sparse+https://index.crates.io/").as_deref(),
        Some("index.crates.io")
    );
    assert_eq!(host_of("not a url"), None);
}

#[test]
fn the_scan_runs_only_with_a_terminal_a_trusted_workspace_and_no_opt_out() {
    assert_eq!(scan_gate(false, true, true), Ok(()));
    assert!(
        scan_gate(true, true, true)
            .unwrap_err()
            .contains("--no-model")
    );
    assert!(
        scan_gate(false, false, true)
            .unwrap_err()
            .contains("no terminal")
    );
    assert!(
        scan_gate(false, true, false)
            .unwrap_err()
            .contains("not trusted")
    );
    // A refusal never depends on the other conditions being met.
    assert!(scan_gate(true, false, false).is_err());
}
