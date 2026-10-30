//! The first layer of setup: facts read from the GitHub API through `gh` and from simple
//! repository files, with no model. Every step is fail-soft: on any failure its field stays
//! `Unknown` or empty and the cause goes to the report.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use fontaine_guardian::{EnvFileFacts, Protection, Visibility};
use futures::future::BoxFuture;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use yaml_rust2::{Yaml, YamlLoader};

use super::{ProjectEnvironment, Registry, RemoteFacts, VERSION};

/// How long one `gh` call may take.
const GH_TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes of a `gh` answer that are read.
const GH_STDOUT_CAP: u64 = 1 << 20;
const GH_STDERR_CAP: u64 = 16 << 10;
/// Bytes of one repository file that are read.
const FILE_CAP: u64 = 256 << 10;
const MISSING_GH: &str = "`gh` is not installed";

/// The result of one `gh` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GhOutcome {
    Output(String),
    /// A non-zero exit, with what `gh` printed on stderr.
    Failed(String),
    Missing,
    TimedOut,
}

/// Runs `gh` with a fixed argument vector. A trait so tests never spawn the real binary.
pub trait Gh: Send + Sync {
    fn run<'a>(&'a self, args: &'a [&'a str]) -> BoxFuture<'a, GhOutcome>;
}

/// The `gh` on `PATH`, run without a shell and without prompts.
pub struct SystemGh;

impl Gh for SystemGh {
    fn run<'a>(&'a self, args: &'a [&'a str]) -> BoxFuture<'a, GhOutcome> {
        Box::pin(async move {
            let mut process = tokio::process::Command::new("gh");
            process
                .args(args)
                .env("GH_PROMPT_DISABLED", "1")
                .env("GH_NO_UPDATE_NOTIFIER", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = match process.spawn() {
                Ok(child) => child,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return GhOutcome::Missing,
                Err(e) => return GhOutcome::Failed(format!("cannot start `gh`: {e}")),
            };
            let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
                return GhOutcome::Failed("`gh` has no output".to_string());
            };
            let work = async {
                let (mut out, mut err) = (Vec::new(), Vec::new());
                let (mut stdout, mut stderr) =
                    (stdout.take(GH_STDOUT_CAP), stderr.take(GH_STDERR_CAP));
                let (a, b) =
                    tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
                a?;
                b?;
                let status = child.wait().await?;
                Ok::<_, std::io::Error>((status, out, err))
            };
            match tokio::time::timeout(GH_TIMEOUT, work).await {
                Err(_) => GhOutcome::TimedOut,
                Ok(Err(e)) => GhOutcome::Failed(format!("`gh` did not finish: {e}")),
                Ok(Ok((status, out, _))) if status.success() => {
                    GhOutcome::Output(String::from_utf8_lossy(&out).into_owned())
                }
                Ok(Ok((_, _, err))) => {
                    GhOutcome::Failed(String::from_utf8_lossy(&err).into_owned())
                }
            }
        })
    }
}

/// What the first layer found, with the inputs of the model scan.
pub struct Discovery {
    pub record: ProjectEnvironment,
    /// `key: value` facts for the scan's project-settings section: GitHub settings and config
    /// values the first layer read, credentials stripped.
    pub settings: BTreeMap<String, String>,
    /// The directory repository-wide files are read from: the git work-tree root, which is not
    /// the primary root when that is a subdirectory.
    pub repo_root: PathBuf,
    /// One line per step that fell back to unknown or empty, and why.
    pub fallbacks: Vec<String>,
}

pub async fn discover(
    gh: &dyn Gh,
    primary_root: &Path,
    git_remotes: &[String],
    discovered_at: String,
) -> Discovery {
    let repo_root =
        crate::trust::work_tree_root(primary_root).unwrap_or_else(|| primary_root.to_path_buf());
    let mut found = Found::default();
    let mut remotes = Vec::new();
    for (position, url) in git_remotes.iter().enumerate() {
        let slug = github_slug(url);
        let facts = match &slug {
            Some(slug) => github_remote(gh, url, slug, position == 0, &mut found).await,
            None => {
                found.fallback(format!(
                    "{url}: not a github.com remote, so visibility and branch protection are unknown"
                ));
                unknown_remote(url, None)
            }
        };
        remotes.push(facts);
    }
    let workflows = workflow_facts(&repo_root);
    let mut registries = Registries::default();
    for dir in distinct([primary_root, repo_root.as_path()]) {
        registries.read(dir);
    }
    let env_files = env_files(primary_root);
    Discovery {
        record: ProjectEnvironment {
            version: VERSION,
            discovered_at,
            remotes,
            tag_release_workflows: workflows.tag_release,
            workflow_environments: workflows.environments,
            package_registries: registries.found,
            env_files,
            stack: stack(primary_root),
            scan: None,
        },
        settings: {
            let mut settings = found.settings;
            settings.extend(registries.settings);
            settings
        },
        repo_root,
        fallbacks: found.fallbacks,
    }
}

fn distinct(dirs: [&Path; 2]) -> Vec<&Path> {
    if dirs[0] == dirs[1] {
        vec![dirs[0]]
    } else {
        dirs.to_vec()
    }
}

#[derive(Default)]
struct Found {
    settings: BTreeMap<String, String>,
    fallbacks: Vec<String>,
}

impl Found {
    fn fallback(&mut self, line: String) {
        if !self.fallbacks.contains(&line) {
            self.fallbacks.push(line);
        }
    }

    fn setting(&mut self, key: &str, value: String) {
        self.settings.insert(key.to_string(), value);
    }
}

fn unknown_remote(url: &str, slug: Option<String>) -> RemoteFacts {
    RemoteFacts {
        url: url.to_string(),
        slug,
        visibility: Visibility::Unknown,
        default_branch: None,
        default_branch_protection: Protection::Unknown,
        branch_rules: Vec::new(),
        deploy_environments: Vec::new(),
    }
}

/// `owner/repo` of a github.com remote URL, in any of the forms git accepts. Anything else, and
/// any slug that could pass for an option, is None.
pub fn github_slug(url: &str) -> Option<String> {
    let rest = if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        host.eq_ignore_ascii_case("github.com").then_some(path)?
    } else {
        let (authority, path) = url.split_once(':')?;
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        host.eq_ignore_ascii_case("github.com").then_some(path)?
    };
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && !part.starts_with('-')
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (valid(owner) && valid(repo)).then(|| format!("{owner}/{repo}"))
}

/// What a failed `gh` call says about the repository's visibility: an org policy that bars the
/// check leaves it unverified, which counts as possibly public; everything else is unknown.
fn failure_visibility(stderr: &str) -> Visibility {
    let lower = stderr.to_lowercase();
    if lower.contains("http 403")
        || lower.contains("saml")
        || lower.contains("resource protected by organization")
    {
        Visibility::Unverified
    } else {
        Visibility::Unknown
    }
}

fn first_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
}

fn failure_text(outcome: &GhOutcome) -> String {
    match outcome {
        GhOutcome::Missing => MISSING_GH.to_string(),
        GhOutcome::TimedOut => format!(
            "`gh` did not answer within {} seconds",
            GH_TIMEOUT.as_secs()
        ),
        GhOutcome::Failed(stderr) => {
            let line = first_line(stderr);
            if line.is_empty() {
                "`gh` failed".to_string()
            } else {
                format!("`gh` failed: {line}")
            }
        }
        GhOutcome::Output(_) => "unexpected output".to_string(),
    }
}

fn json_of(outcome: &GhOutcome) -> Option<Value> {
    match outcome {
        GhOutcome::Output(text) => serde_json::from_str(text).ok(),
        _ => None,
    }
}

fn parse_visibility(text: &str) -> Option<Visibility> {
    match text.to_ascii_lowercase().as_str() {
        "public" => Some(Visibility::Public),
        "private" => Some(Visibility::Private),
        "internal" => Some(Visibility::Internal),
        _ => None,
    }
}

/// A branch name that is safe to put in an API path.
fn safe_branch(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

async fn github_remote(
    gh: &dyn Gh,
    url: &str,
    slug: &str,
    primary: bool,
    found: &mut Found,
) -> RemoteFacts {
    let mut facts = unknown_remote(url, Some(slug.to_string()));
    let view = gh
        .run(&[
            "repo",
            "view",
            slug,
            "--json",
            "visibility,defaultBranchRef",
        ])
        .await;
    let parsed = json_of(&view);
    let visibility = parsed
        .as_ref()
        .and_then(|v| v["visibility"].as_str())
        .and_then(parse_visibility);
    match (&view, visibility) {
        (_, Some(visibility)) => facts.visibility = visibility,
        (GhOutcome::Failed(stderr), None) => {
            facts.visibility = failure_visibility(stderr);
            found.fallback(format!("{slug}: visibility {}", failure_text(&view)));
        }
        _ => found.fallback(format!("{slug}: visibility {}", failure_text(&view))),
    }
    facts.default_branch = parsed
        .as_ref()
        .and_then(|v| v["defaultBranchRef"]["name"].as_str())
        .filter(|name| safe_branch(name))
        .map(str::to_string);
    if let Some(branch) = facts.default_branch.clone() {
        protection(gh, slug, &branch, &mut facts, found).await;
    }
    if parsed.is_some() {
        facts.deploy_environments = environments(gh, slug, found).await;
    }
    if primary {
        settings(gh, slug, &facts, found).await;
    }
    facts
}

async fn protection(
    gh: &dyn Gh,
    slug: &str,
    branch: &str,
    facts: &mut RemoteFacts,
    found: &mut Found,
) {
    let branch_call = gh
        .run(&["api", &format!("repos/{slug}/branches/{branch}")])
        .await;
    let rules_call = gh
        .run(&["api", &format!("repos/{slug}/rules/branches/{branch}")])
        .await;
    let protected = json_of(&branch_call).and_then(|v| v["protected"].as_bool());
    let rules: Option<Vec<String>> = json_of(&rules_call).and_then(|v| {
        v.as_array().map(|rules| {
            let mut types: Vec<String> = rules
                .iter()
                .filter_map(|rule| rule["type"].as_str().map(str::to_string))
                .collect();
            types.sort();
            types.dedup();
            types
        })
    });
    facts.default_branch_protection = match (protected, &rules) {
        (Some(true), _) => Protection::Protected,
        (_, Some(rules)) if !rules.is_empty() => Protection::Protected,
        (Some(false), Some(_)) => Protection::Unprotected,
        _ => Protection::Unknown,
    };
    facts.branch_rules = rules.unwrap_or_default();
    if facts.default_branch_protection == Protection::Unknown {
        let cause = if protected.is_none() {
            failure_text(&branch_call)
        } else {
            failure_text(&rules_call)
        };
        found.fallback(format!("{slug}: branch protection {cause}"));
    }
}

async fn environments(gh: &dyn Gh, slug: &str, found: &mut Found) -> Vec<String> {
    let call = gh
        .run(&["api", &format!("repos/{slug}/environments")])
        .await;
    let Some(value) = json_of(&call) else {
        found.fallback(format!(
            "{slug}: deploy environments {}",
            failure_text(&call)
        ));
        return Vec::new();
    };
    let mut names: Vec<String> = value["environments"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|env| env["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The project-settings facts the scan may cite.
async fn settings(gh: &dyn Gh, slug: &str, facts: &RemoteFacts, found: &mut Found) {
    let visibility = match &facts.visibility {
        Visibility::Public => "public",
        Visibility::Private => "private",
        Visibility::Internal => "internal",
        Visibility::Unverified => "unverified",
        Visibility::Unknown | Visibility::Expired(_) => "unknown",
    };
    found.setting("github.visibility", visibility.to_string());
    if let Some(branch) = &facts.default_branch {
        found.setting("github.default_branch", branch.clone());
    }
    if !facts.deploy_environments.is_empty() {
        found.setting("github.environments", facts.deploy_environments.join(", "));
    }
    let call = gh
        .run(&[
            "repo",
            "view",
            slug,
            "--json",
            "homepageUrl,isFork,parent,repositoryTopics",
        ])
        .await;
    if let Some(value) = json_of(&call) {
        if let Some(home) = value["homepageUrl"].as_str().filter(|h| !h.is_empty())
            && let Some(host) = host_of(home)
        {
            found.setting("github.homepage", host);
        }
        if value["isFork"].as_bool() == Some(true) {
            let owner = value["parent"]["owner"]["login"].as_str();
            let name = value["parent"]["name"].as_str();
            if let (Some(owner), Some(name)) = (owner, name) {
                found.setting("github.fork_of", format!("{owner}/{name}"));
            }
        }
        let topics: Vec<String> = value["repositoryTopics"]
            .as_array()
            .map(|all| {
                all.iter()
                    .filter_map(|topic| {
                        topic["name"]
                            .as_str()
                            .or_else(|| topic["topic"]["name"].as_str())
                            .or_else(|| topic.as_str())
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !topics.is_empty() {
            found.setting("github.topics", topics.join(", "));
        }
    } else {
        found.fallback(format!(
            "{slug}: repository settings {}",
            failure_text(&call)
        ));
    }
    let pages = gh.run(&["api", &format!("repos/{slug}/pages")]).await;
    match (&pages, json_of(&pages)) {
        (_, Some(value)) => {
            if let Some(url) = value["html_url"].as_str().and_then(host_of) {
                found.setting("github.pages.url", url);
            }
            if let Some(cname) = value["cname"].as_str().filter(|c| !c.is_empty()) {
                found.setting("github.pages.cname", cname.to_string());
            }
        }
        // No Pages site is a normal answer, not a fallback.
        (GhOutcome::Failed(stderr), None) if stderr.contains("404") => {}
        _ => found.fallback(format!("{slug}: Pages site {}", failure_text(&pages))),
    }
}

/// The host of a URL, lower-cased, with no userinfo, port, path, query or fragment. A leading
/// `sparse+` or `git+` is dropped.
pub fn host_of(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(['"', '\'']);
    let value = value
        .strip_prefix("sparse+")
        .or_else(|| value.strip_prefix("git+"))
        .unwrap_or(value);
    let url = url::Url::parse(value).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

#[derive(Default)]
struct Workflows {
    tag_release: Vec<String>,
    environments: Vec<String>,
}

fn read_capped(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut text = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(FILE_CAP)
        .read_to_end(&mut text)
        .ok()?;
    String::from_utf8(text).ok()
}

fn workflow_facts(repo_root: &Path) -> Workflows {
    let dir = repo_root.join(".github").join("workflows");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".yml") || name.ends_with(".yaml"))
        .collect();
    names.sort();
    let mut out = Workflows::default();
    let mut environments = BTreeSet::new();
    for name in names {
        let Some(text) = read_capped(&dir.join(&name)) else {
            continue;
        };
        let parsed = parse_workflow(&text);
        if parsed.tag_release {
            out.tag_release.push(format!(".github/workflows/{name}"));
        }
        environments.extend(parsed.environments);
    }
    out.environments = environments.into_iter().collect();
    out
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct ParsedWorkflow {
    pub tag_release: bool,
    pub environments: Vec<String>,
}

/// A workflow that does not parse yields nothing.
pub fn parse_workflow(text: &str) -> ParsedWorkflow {
    let Ok(docs) = YamlLoader::load_from_str(text) else {
        return ParsedWorkflow::default();
    };
    let Some(doc) = docs.first() else {
        return ParsedWorkflow::default();
    };
    // YAML 1.1 reads a bare `on` key as a boolean.
    let on = match doc.as_hash() {
        Some(hash) => hash
            .get(&Yaml::String("on".to_string()))
            .or_else(|| hash.get(&Yaml::Boolean(true))),
        None => None,
    };
    let tag_release = on.is_some_and(triggers_release);
    let mut environments = Vec::new();
    if let Some(jobs) = doc["jobs"].as_hash() {
        for job in jobs.values() {
            let environment = &job["environment"];
            let name = environment
                .as_str()
                .or_else(|| environment["name"].as_str());
            if let Some(name) = name {
                environments.push(name.to_string());
            }
        }
    }
    environments.sort();
    environments.dedup();
    ParsedWorkflow {
        tag_release,
        environments,
    }
}

fn triggers_release(on: &Yaml) -> bool {
    let named = |event: &str| matches!(event, "release" | "create");
    match on {
        Yaml::String(event) => named(event),
        Yaml::Array(events) => events.iter().any(|e| e.as_str().is_some_and(named)),
        Yaml::Hash(events) => events.iter().any(|(event, config)| match event.as_str() {
            Some(event) if named(event) => true,
            Some("push") => {
                config["tags"].as_vec().is_some_and(|tags| !tags.is_empty())
                    || config["tags"].as_str().is_some()
            }
            _ => false,
        }),
        _ => false,
    }
}

#[derive(Default)]
struct Registries {
    found: Vec<Registry>,
    settings: BTreeMap<String, String>,
}

impl Registries {
    fn add(&mut self, file: &str, key: String, value: &str) {
        let Some(host) = host_of(value) else { return };
        self.settings
            .insert(format!("config:{file}:{key}"), host.clone());
        if !self.found.iter().any(|r| r.host == host) {
            self.found.push(Registry {
                host,
                file: file.to_string(),
            });
        }
    }

    fn read(&mut self, dir: &Path) {
        self.npmrc(dir);
        self.yarnrc(dir);
        self.pip(dir);
        self.pyproject(dir);
        self.cargo(dir);
    }

    fn npmrc(&mut self, dir: &Path) {
        let Some(text) = read_capped(&dir.join(".npmrc")) else {
            return;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with(['#', ';']) {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                if key == "registry" || key.ends_with(":registry") {
                    self.add(".npmrc", key.to_string(), value);
                }
            }
        }
    }

    fn yarnrc(&mut self, dir: &Path) {
        let Some(text) = read_capped(&dir.join(".yarnrc.yml")) else {
            return;
        };
        let Ok(docs) = YamlLoader::load_from_str(&text) else {
            return;
        };
        let Some(doc) = docs.first() else { return };
        if let Some(server) = doc["npmRegistryServer"].as_str() {
            self.add(".yarnrc.yml", "npmRegistryServer".to_string(), server);
        }
        for (scope, config) in doc["npmScopes"].as_hash().into_iter().flatten() {
            if let (Some(scope), Some(server)) =
                (scope.as_str(), config["npmRegistryServer"].as_str())
            {
                self.add(
                    ".yarnrc.yml",
                    format!("npmScopes.{scope}.npmRegistryServer"),
                    server,
                );
            }
        }
    }

    fn pip(&mut self, dir: &Path) {
        for file in ["pip.conf", "pip.ini"] {
            let Some(text) = read_capped(&dir.join(file)) else {
                continue;
            };
            let mut in_list: Option<&str> = None;
            for line in text.lines() {
                let continued = line.starts_with([' ', '\t']);
                let line = line.trim();
                if line.starts_with(['#', ';']) || line.is_empty() {
                    continue;
                }
                if continued && let Some(key) = in_list {
                    self.add(file, key.to_string(), line);
                    continue;
                }
                in_list = None;
                if let Some((key, value)) = line.split_once(['=', ':']) {
                    let key = key.trim();
                    let key = match key {
                        "index-url" => "index-url",
                        "extra-index-url" => "extra-index-url",
                        _ => continue,
                    };
                    in_list = Some(key);
                    if !value.trim().is_empty() {
                        self.add(file, key.to_string(), value);
                    }
                }
            }
        }
    }

    fn pyproject(&mut self, dir: &Path) {
        let Some(text) = read_capped(&dir.join("pyproject.toml")) else {
            return;
        };
        let Ok(doc) = text.parse::<toml::Table>() else {
            return;
        };
        let tool = doc.get("tool").and_then(|t| t.as_table());
        let uv = tool.and_then(|t| t.get("uv")).and_then(|u| u.as_table());
        let mut urls: Vec<(String, String)> = Vec::new();
        if let Some(uv) = uv {
            if let Some(url) = uv.get("index-url").and_then(|u| u.as_str()) {
                urls.push(("tool.uv.index-url".to_string(), url.to_string()));
            }
            for (i, url) in uv
                .get("extra-index-url")
                .and_then(|u| u.as_array())
                .into_iter()
                .flatten()
                .filter_map(|u| u.as_str())
                .enumerate()
            {
                urls.push((format!("tool.uv.extra-index-url[{i}]"), url.to_string()));
            }
            for (i, index) in uv
                .get("index")
                .and_then(|i| i.as_array())
                .into_iter()
                .flatten()
                .enumerate()
            {
                if let Some(url) = index.get("url").and_then(|u| u.as_str()) {
                    urls.push((format!("tool.uv.index[{i}]"), url.to_string()));
                }
            }
        }
        let sources = tool
            .and_then(|t| t.get("poetry"))
            .and_then(|p| p.get("source"))
            .and_then(|s| s.as_array());
        for (i, source) in sources.into_iter().flatten().enumerate() {
            if let Some(url) = source.get("url").and_then(|u| u.as_str()) {
                urls.push((format!("tool.poetry.source[{i}]"), url.to_string()));
            }
        }
        for (key, url) in urls {
            self.add("pyproject.toml", key, &url);
        }
    }

    fn cargo(&mut self, dir: &Path) {
        for file in [".cargo/config.toml", ".cargo/config"] {
            let Some(text) = read_capped(&dir.join(file)) else {
                continue;
            };
            let Ok(doc) = text.parse::<toml::Table>() else {
                continue;
            };
            for (table, field) in [("registries", "index"), ("source", "registry")] {
                let entries = doc.get(table).and_then(|t| t.as_table());
                for (name, entry) in entries.into_iter().flatten() {
                    if let Some(url) = entry.get(field).and_then(|u| u.as_str()) {
                        self.add(file, format!("{table}.{name}.{field}"), url);
                    }
                }
            }
        }
    }
}

/// `.env*` files at the top and one level down, with whether git ignores them. Contents are
/// never read.
fn env_files(root: &Path) -> Vec<EnvFileFacts> {
    let mut present = Vec::new();
    let mut dirs = vec![(root.to_path_buf(), String::new())];
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|t| t.is_dir())
                && !matches!(name.as_str(), ".git" | "node_modules" | "target")
            {
                dirs.push((entry.path(), format!("{name}/")));
            }
        }
    }
    for (dir, prefix) in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_env_like(&name) && entry.file_type().is_ok_and(|t| t.is_file()) {
                present.push(format!("{prefix}{name}"));
            }
        }
    }
    if present.is_empty() {
        return Vec::new();
    }
    let visible: BTreeSet<String> = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_global(false)
        .require_git(false)
        .max_depth(Some(2))
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
        .flatten()
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(root).ok()?;
            Some(relative.to_string_lossy().replace('\\', "/"))
        })
        .collect();
    present.sort();
    present
        .into_iter()
        .map(|name| EnvFileFacts {
            gitignored: !visible.contains(&name),
            name,
        })
        .collect()
}

/// Whether a file name marks a file of environment variables, whose contents are never sent
/// anywhere.
pub fn is_env_like(name: &str) -> bool {
    let name = name.rsplit('/').next().unwrap_or(name);
    name == ".env" || name.starts_with(".env.") || name.ends_with(".env")
}

fn stack(root: &Path) -> Vec<String> {
    const MANIFESTS: &[(&str, &str)] = &[
        ("Cargo.toml", "rust"),
        ("package.json", "node"),
        ("deno.json", "deno"),
        ("pyproject.toml", "python"),
        ("requirements.txt", "python"),
        ("go.mod", "go"),
        ("Gemfile", "ruby"),
        ("pom.xml", "java"),
        ("build.gradle", "java"),
        ("build.gradle.kts", "java"),
        ("composer.json", "php"),
        ("mix.exs", "elixir"),
        ("Package.swift", "swift"),
    ];
    let mut out: Vec<String> = Vec::new();
    for (file, name) in MANIFESTS {
        if root.join(file).is_file() && !out.iter().any(|n| n == name) {
            out.push((*name).to_string());
        }
    }
    out
}
