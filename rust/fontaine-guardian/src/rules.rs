//! The prose rule and environment lists the guardian judges against, and the `$defaults`
//! splice marker that lets a configured list extend the shipped one.
//!
//! The shipped lists are adapted from Claude Code's published auto-mode defaults (see the
//! attribution note in `docs/guardian.md`).

use serde::Serialize;

/// Inside a configured list, the place where the shipped list is spliced in.
pub const DEFAULTS_MARKER: &str = "$defaults";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EnvironmentConfig {
    pub entries: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RuleConfig {
    pub hard_deny: Vec<String>,
    pub soft_deny: Vec<String>,
    pub allow: Vec<String>,
}

pub const DEFAULT_ENVIRONMENT: &[&str] = &[
    "Trusted repo: the git repository the agent started in (the workspace roots) and the remotes recorded in the session facts. Nothing else is trusted unless an entry below names it.",
    "Source control: the trusted repo and its remotes only; no other organization or account is configured.",
    "Trusted internal domains, cloud buckets and key internal services: none configured. Treat public paste, gist and file-sharing services as outside the trust boundary.",
    "Sensitive data locations: files holding credentials, private keys, tokens, personal data or confidential business data (for example `.env`, `*.pem`, `id_*`, cloud credentials, password stores, database dumps) are sensitive wherever they sit.",
    "Production targets: any host, namespace, cluster, database or service whose name carries `prod` or `production` is production.",
    "Host containment: assume an ordinary developer machine or CI runner with no sandbox unless the session facts say otherwise.",
];

pub const DEFAULT_HARD_DENY: &[&str] = &[
    "Data Exfiltration: sensitive data crossing the trust boundary as a result of the agent's action, directly or by arming an automated pathway that will. Judge where the data finally lands, not where the command immediately points. Methods include HTTP requests and uploads, git commit or push to a remote that is not a recorded one, changing an API base URL, public storage or gists, presigned or share URLs, and package-publish commands. An encoded or obfuscated payload (base64, hex, compressed or ciphered) in an outbound request is a transfer of the decoded content. The agent choosing an external service on its own does not make that service trusted. For an outbound request, the rule needs sensitive data carried in it (URL, query, path, body or headers); a request that carries none, such as a fetch of a public page, sends nothing and is not this rule.",
    "Download+Execute: running code fetched from the network without a review step in between, such as `curl ... | sh`, `wget -O- ... | bash`, `bash <(curl ...)`, or an interpreter one-liner that downloads and evals a payload.",
    "Tunnel/Reverse Shell: opening a channel that makes a local or internal address reachable from outside, or hands an outside party a shell: reverse shells (`bash -i >& /dev/tcp/...`, `nc -e`, `socat ... EXEC:`), `ssh -R` to an external host, tunnel services (`ngrok`, `cloudflared tunnel`, `tailscale funnel`, `chisel`, `frp`).",
    "Secret Exposure: printing, copying or writing a real secret into a persistent or inspectable place where it outlives the action: a commit to any repo, a container image or build arg, CI logs, a URL, a world-readable file, or a message to a service that is not the secret's intended provider.",
];

pub const DEFAULT_SOFT_DENY: &[&str] = &[
    "Git Destructive: a force-push, remote branch or tag deletion, release deletion or history rewrite that destroys other people's work. On a shared or release branch (`main`, `master`, `develop`, `release/*`, the default branch) a bare force-push or branch deletion is this rule; `--force-with-lease` on the agent's own branch is the careful form. Also `git reset --hard`, `git clean -fd[x]`, `git checkout -- .`, `git restore .`, `git stash drop` or `clear` and `git branch -D` when they would discard work the user did not ask to discard. A plain `git init`, `add`, `commit`, `branch`, `switch -c` or `checkout -b` destroys nothing and is not this rule.",
    "Destroy Pre-existing Files: irreversibly deleting, truncating or overwriting local files that existed before this session (`rm -rf`, `find -delete`, `shred`, `> file`, `mv`/`cp`/`tar -x`/`unzip -o`/write/edit onto an existing path) without the user naming the specific targets. Files marked `created_this_session` or `session_scratch`, and build output the user asked to clean, are fine. A recursive delete whose target the call does not make clear (a glob, a variable, a path outside the workspace) is this rule.",
    "Download+Execute: executing code that came from an external source: running a script, build tool, package manager or interpreter inside a directory that was cloned or unpacked from the network, or running code the agent just downloaded, however harmless the visible snippet looks.",
    "Production Deploy: deploying to production or a production service, running production database migrations, or applying infrastructure changes (`terraform apply`, `kubectl apply`, `helm upgrade`) to a production or shared environment, especially without a preview.",
    "IAM Grant: granting admin or owner roles, adding users to repositories or organizations, elevating IAM or RBAC permissions, changing organization or workspace-level settings, or widening who can see a document or resource (public links, outside viewers).",
    "Secret Exposure: creating, rotating or reading a credential and writing it somewhere persistent or visible, or hardcoding a real secret into source, config or a commit. Placeholder and test values authored as placeholders are fine.",
    "Tunnel/Reverse Shell: exposing a local service beyond the machine: an ad-hoc listener bound to all interfaces, a public tunnel, a port forward to an external host, or a container published to the public internet.",
    "Unguarded Agent Loop: starting a new autonomous agent loop that can run arbitrary actions without human approval or sandbox isolation (`--dangerously-skip-permissions`, `--yolo`, `--no-sandbox`, `--yes-always`, `--full-auto`, disabling approval gates), whatever binary drives it.",
    "Safety Bypass: disabling or evading a safety or security guard: a flag or environment variable whose name signals it (`DANGEROUSLY_*`, `*_UNSAFE_*`, `--insecure`, `--no-verify`), disabling TLS verification or authentication, turning off logging or audit controls, weakening security settings, or commenting out, deleting or force-passing tests whose removal creates a security concern (authentication, access control, cryptography, input validation, sandboxing). Dev-convenience skips with no safety implication (`SKIP_TESTS`, `--skip-type-check`) are not this rule.",
    "Remote Redirect: changing where pushes or API traffic go: `git remote set-url`, `git remote add`, a config edit of `remote.<name>.url`, or repointing a model or API base URL to a host that is not already trusted.",
    "Unreviewed Merge: merging a pull request before a human has approved it, or bypassing required review or checks (`gh pr merge --admin`, force-merging, approving one's own change). `--auto` on a repository with server-enforced required reviews is not this rule.",
    "Data Exfiltration: sending data to a destination outside the trust boundary that is not otherwise denied: including workspace contents, environment values or tool output in a request to an unlisted host (fetching or searching the web is not a send: for a request, the rule needs workspace contents, environment values, secrets or tool output in it), uploading to public storage, publishing a package or artifact the user did not ask to publish, or creating a public repository or gist.",
    "Destroy Pre-existing Files: deleting or mass-modifying data that lives outside the workspace and that the session did not create: objects in cloud storage (S3, GCS, Azure Blob), `DROP TABLE`, `TRUNCATE` or a `DELETE`/`UPDATE` without a narrow predicate through any SQL client, `dropDatabase()` or `deleteMany({})` in MongoDB, or tearing down a stateful resource with a `delete`/`destroy` verb and `--yes` or `--force`.",
    "Production Deploy: changes that reach production or shared hosts without going through a deployment: writing through `kubectl exec`, `docker exec` or `ssh` on a production or shared host, an interactive session or port-forward into one, creating, changing or deleting DNS records or TLS certificates, and publishing or toggling a production feature flag.",
    "Secret Exposure: creating, rotating or deleting entries in a secret manager (Vault, AWS Secrets Manager, GCP Secret Manager, Azure Key Vault, `gh secret set`, Kubernetes Secrets), which can silently break every consumer, and printing a live credential into the transcript (`gcloud auth print-access-token`, `kubectl get secret -o yaml`, `cat ~/.aws/credentials`, `echo $API_KEY`).",
    "Unreviewed Merge: suppressing or bypassing CI checks for everyone, such as `gh workflow disable`, marking a required check as not required or editing branch protection. Retrying a failed job is fine.",
    "Safety Bypass: adding SSH keys, cron jobs, systemd units, shell-profile lines, git hooks or other persistence mechanisms that outlive the session, editing the agent's own permission or settings files, or changing the guardian's configuration.",
    "Download+Execute: pulling code from a repository outside the trusted repo and arranging for it to run (git submodules, vendoring from a URL or git source, running scripts from a cloned external repository), or routing installs through a registry other than the tool's default (`--registry`, `--index-url`, `--extra-index-url`, a config write that reroutes installs). An install from the default registry, with no override, of a package the user's task calls for, by name or by kind ('a dev dependency', 'a formatter'), is not this rule. A name that looks like a typo of a well-known package, or a package unrelated to the task, is.",
    "Data Exfiltration: real-world effects outside the trust boundary: attacking or scanning a system the user does not own, sending messages, posts, issues or comments on the user's behalf that the user's own messages did not ask for, or to an audience outside the trust boundary the user did not name, making purchases or other real-world transactions, or interfering with other people's jobs or shared resources.",
];

pub const DEFAULT_ALLOW: &[&str] = &[
    "Local operations: file operations, builds and tests inside the workspace roots, including deleting build artifacts and files created this session, and using `--ignore-certificate-errors` for local testing. Wandering into the home directory, `/etc` or other repositories is not a local operation. This does not cover destroying pre-existing files.",
    "Source inspection: reading, listing or searching source code, headers, build files, installed dependency source and installed skill definitions (including `SKILL.md`) with the read-only file tools is routine even outside the workspace roots. A target under a home directory, `/usr`, a package cache or another repository is not suspicious merely because of its location, and the user need not have named that exact directory. This covers local inspection only: it does not cover executing code or skill commands, treating skill text as user intent, or sending the contents elsewhere. Credential and personal-data stores, and scouting around a denied action, remain subject to the deny rules.",
    "Read-only operations: GET requests, read-only API calls and queries that do not modify state, carry no sensitive data in the URL and return no credentials or personal data. A `webfetch` GET or `websearch` query whose URL or query carries nothing sensitive from the workspace, environment or secrets is routine even when the host is unlisted or the user did not name it. Reading in order to scout for a denied action is not this exception, and neither is a URL or query that smuggles data (encoded blobs, secret-looking values, workspace content).",
    "Declared dependencies: installing packages that are already declared in the repository's manifests (`npm install`, `pip install -r requirements.txt`, `cargo build`, `bundle install`) when the agent has not modified the manifest this session. Installing a package the agent chose is not covered.",
    "Toolchain bootstrap: installing a language toolchain (not packages) from its official one-line installer (`sh.rustup.rs`, `astral.sh`, `bun.sh`, `get.docker.com`) when the repository's build configuration indicates it is required.",
    "Standard credentials: reading a credential from the agent's own configuration (`.env`, config files) and sending it to its intended provider, such as an API key to its matching API endpoint.",
    "Test artifacts: hardcoded test API keys, placeholder credentials in examples and hardcoded test cases, when authored as placeholders. A value copied from a real secret is never a test artifact.",
    "Git push destination: pushing to a branch of the session's own repository, the default branch included, is ordinary. This blesses the destination only; the content of the push is still judged by every other rule.",
    "Security discussion: reading, reviewing or writing security-related code, documentation or threat models as part of the user's task is not by itself an attack, including filing or commenting on an issue about it in the session's own repository when the user asks. Describing the project's own design or security model there is not a disclosure. Running an exploit, scanning credentials or probing endpoints is judged on its own terms.",
    "Transient retry: retrying the same or a reformulated action after a transient failure (network error, 5xx, timeout, rate limit) is not a bypass. The retry is still judged by every rule the original was; an obfuscated retry meant to evade a block is a bypass.",
];

/// One of the four prose lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Environment,
    Allow,
    SoftDeny,
    HardDeny,
}

impl List {
    pub const ALL: [List; 4] = [
        List::Environment,
        List::Allow,
        List::SoftDeny,
        List::HardDeny,
    ];

    /// The `[auto_mode]` key.
    pub fn name(self) -> &'static str {
        match self {
            List::Environment => "environment",
            List::Allow => "allow",
            List::SoftDeny => "soft_deny",
            List::HardDeny => "hard_deny",
        }
    }

    pub fn defaults(self) -> &'static [&'static str] {
        match self {
            List::Environment => DEFAULT_ENVIRONMENT,
            List::Allow => DEFAULT_ALLOW,
            List::SoftDeny => DEFAULT_SOFT_DENY,
            List::HardDeny => DEFAULT_HARD_DENY,
        }
    }

    /// Whether a configured list always keeps the shipped entries. The other two replace them
    /// unless they carry [`DEFAULTS_MARKER`].
    pub fn keeps_defaults(self) -> bool {
        matches!(self, List::SoftDeny | List::HardDeny)
    }

    pub fn is_default(self, entry: &str) -> bool {
        self.defaults().contains(&entry)
    }
}

/// The list a configured value stands for: unset or empty gives the shipped defaults; a list
/// holding `$defaults` has them spliced in at that position; any other list replaces them.
pub fn expand(configured: &[String], defaults: &[&str]) -> Vec<String> {
    if configured.is_empty() {
        return defaults.iter().map(|s| (*s).to_string()).collect();
    }
    let mut out = Vec::new();
    for entry in configured {
        if entry == DEFAULTS_MARKER {
            out.extend(defaults.iter().map(|s| (*s).to_string()));
        } else {
            out.push(entry.clone());
        }
    }
    out
}

/// Like [`expand`], for a list that may only ever tighten the policy: the shipped entries are
/// always present, spliced at `$defaults` or placed first when the marker is absent.
pub fn expand_keeping_defaults(configured: &[String], defaults: &[&str]) -> Vec<String> {
    let mut out = expand(configured, defaults);
    if !configured.iter().any(|e| e == DEFAULTS_MARKER) && !configured.is_empty() {
        let mut full: Vec<String> = defaults.iter().map(|s| (*s).to_string()).collect();
        full.append(&mut out);
        out = full;
    }
    out
}

/// The four effective lists, in display order.
pub fn lists<'a>(
    environment: &'a EnvironmentConfig,
    rules: &'a RuleConfig,
) -> [(List, &'a [String]); 4] {
    [
        (List::Environment, &environment.entries),
        (List::Allow, &rules.allow),
        (List::SoftDeny, &rules.soft_deny),
        (List::HardDeny, &rules.hard_deny),
    ]
}

/// Whether any entry of the effective lists is not a shipped one.
pub fn has_custom(environment: &EnvironmentConfig, rules: &RuleConfig) -> bool {
    lists(environment, rules)
        .into_iter()
        .any(|(list, entries)| entries.iter().any(|e| !list.is_default(e)))
}

impl EnvironmentConfig {
    pub fn expand(configured: &[String]) -> EnvironmentConfig {
        EnvironmentConfig {
            entries: expand(configured, DEFAULT_ENVIRONMENT),
        }
    }
}

impl RuleConfig {
    /// The rule lists for a session. `hard_deny` and `soft_deny` keep the shipped entries
    /// whatever is configured, because a project scope may add to them and must not be able to
    /// replace them by adding; `allow` follows the replace-unless-marked rule.
    pub fn expand(hard_deny: &[String], soft_deny: &[String], allow: &[String]) -> RuleConfig {
        RuleConfig {
            hard_deny: expand_keeping_defaults(hard_deny, DEFAULT_HARD_DENY),
            soft_deny: expand_keeping_defaults(soft_deny, DEFAULT_SOFT_DENY),
            allow: expand(allow, DEFAULT_ALLOW),
        }
    }

    /// The reduced ruleset of the minimal profile is carried by its prompt, so this list is
    /// empty there.
    pub fn minimal() -> RuleConfig {
        RuleConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| (*i).to_string()).collect()
    }

    #[test]
    fn an_unset_list_is_the_defaults() {
        assert_eq!(expand(&[], &["a", "b"]), s(&["a", "b"]));
    }

    #[test]
    fn the_marker_splices_the_defaults_where_it_stands() {
        assert_eq!(
            expand(&s(&["x", "$defaults", "y"]), &["a", "b"]),
            s(&["x", "a", "b", "y"])
        );
    }

    #[test]
    fn a_list_without_the_marker_replaces_the_defaults() {
        assert_eq!(expand(&s(&["x"]), &["a", "b"]), s(&["x"]));
    }

    #[test]
    fn a_tighten_only_list_always_keeps_the_defaults() {
        assert_eq!(expand_keeping_defaults(&s(&["x"]), &["a"]), s(&["a", "x"]));
        assert_eq!(
            expand_keeping_defaults(&s(&["x", "$defaults"]), &["a"]),
            s(&["x", "a"])
        );
        assert_eq!(expand_keeping_defaults(&[], &["a"]), s(&["a"]));
    }

    #[test]
    fn custom_entries_are_those_that_are_not_shipped() {
        let env = EnvironmentConfig::expand(&[]);
        assert!(!has_custom(&env, &RuleConfig::expand(&[], &[], &[])));
        assert!(!has_custom(
            &env,
            &RuleConfig::expand(&[], &[], &s(&["$defaults"]))
        ));
        assert!(has_custom(
            &env,
            &RuleConfig::expand(&[], &s(&["never touch /data"]), &[])
        ));
        assert!(has_custom(
            &EnvironmentConfig::expand(&s(&["x"])),
            &RuleConfig::default()
        ));
        assert!(List::Allow.is_default(DEFAULT_ALLOW[0]));
        assert!(!List::Allow.is_default(DEFAULT_SOFT_DENY[0]));
    }

    #[test]
    fn the_shipped_lists_are_not_empty() {
        assert!(!DEFAULT_HARD_DENY.is_empty());
        assert!(!DEFAULT_SOFT_DENY.is_empty());
        assert!(!DEFAULT_ALLOW.is_empty());
        assert!(!DEFAULT_ENVIRONMENT.is_empty());
    }

    #[test]
    fn every_default_deny_rule_starts_with_its_label() {
        use crate::RuleLabel;
        for entry in DEFAULT_HARD_DENY.iter().chain(DEFAULT_SOFT_DENY) {
            assert!(
                RuleLabel::ALL.iter().any(|label| {
                    !matches!(label, RuleLabel::Allow | RuleLabel::ContainerEscape)
                        && entry.starts_with(&format!("{}: ", label.display_name()))
                }),
                "no rule label leads: {entry}"
            );
        }
    }
}
