//! Model roles: user-named, ordered lists of models, and the chain that serves one list with the
//! entry currently in use.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fontaine_agents::registry::{ModelSpec, valid_role_name};
use fontaine_tools::lock::lock;

use crate::config::{Config, ConfigError, MainSource};
use crate::models::{ModelRef, known_accounts};

/// The role every session reads for its main model.
pub const MAIN_ROLE: &str = "main";
/// The roles the auxiliary jobs read.
pub const CRITIQUE_ROLE: &str = "critique";
pub const SETUP_ROLE: &str = "setup";
pub const COMPACTION_ROLE: &str = "compaction";

/// The config's roles with each list flattened to concrete references.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roles {
    flat: BTreeMap<String, Vec<ModelRef>>,
}

impl Roles {
    /// Folds `[model] model` into `roles["main"]`, then validates every role against the
    /// accounts. The accounts of an entry that came from `[model] model` are checked when the
    /// model is resolved, as they were before roles existed.
    pub fn from_config(config: &Config) -> Result<Roles, ConfigError> {
        let mut raw = config.roles.clone();
        let mut unchecked_accounts = Vec::new();
        if let (MainSource::ModelKeyString | MainSource::ModelKeyList, Some(model)) =
            (config.main_source, &config.model)
        {
            raw.insert(MAIN_ROLE.to_string(), model.entries().to_vec());
            unchecked_accounts.push(MAIN_ROLE);
        }
        for (name, list) in &raw {
            valid_role_name(name)
                .map_err(|why| ConfigError::model(format!("[models.roles]: {why}")))?;
            if list.is_empty() {
                return Err(ConfigError::model(format!(
                    "[models.roles] `{name}`: a role names at least one model"
                )));
            }
        }
        let mut flat = BTreeMap::new();
        for name in raw.keys() {
            flatten_role(
                name,
                &raw,
                config,
                &unchecked_accounts,
                &mut vec![name.clone()],
                &mut flat,
            )?;
        }
        Ok(Roles { flat })
    }

    pub fn get(&self, name: &str) -> Option<&[ModelRef]> {
        self.flat.get(name).map(Vec::as_slice)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.flat.contains_key(name)
    }

    pub fn is_empty(&self) -> bool {
        self.flat.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.flat.keys().map(String::as_str)
    }

    /// One line per role, as the `agent` and `models` tools list them:
    /// ``- `coding`: anthropic/claude-sonnet-5-5, kimi/kimi-for-coding``.
    pub fn describe(&self) -> String {
        self.flat
            .iter()
            .map(|(name, entries)| format!("- `{name}`: {}", join(entries)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The references a list of entries stands for: `<account>/<model>` as itself, a role name
    /// or `$main` as that role's list, each reference once at its first position.
    pub fn flatten_entries(&self, entries: &[String]) -> Result<Vec<ModelRef>, String> {
        let mut out: Vec<ModelRef> = Vec::new();
        for entry in entries {
            let spliced: Vec<ModelRef> = if entry.contains('/') {
                vec![ModelRef::parse(entry).ok_or_else(|| {
                    format!("`{entry}` is not a model: expected `<account>/<model>`")
                })?]
            } else {
                let name = if entry == "$main" { MAIN_ROLE } else { entry };
                self.get(name).ok_or_else(|| self.unknown(name))?.to_vec()
            };
            for reference in spliced {
                if !out.contains(&reference) {
                    out.push(reference);
                }
            }
        }
        Ok(out)
    }

    fn unknown(&self, name: &str) -> String {
        if self.is_empty() {
            format!("`{name}` is not a defined model role; define roles under `[models.roles]`")
        } else {
            format!(
                "`{name}` is not a defined model role; defined roles: {}",
                self.names().collect::<Vec<_>>().join(", ")
            )
        }
    }

    /// The role label and references a spec stands for. `$guardian_default` has no list.
    pub fn entries_of(&self, spec: &ModelSpec) -> Result<(Option<String>, Vec<ModelRef>), String> {
        match spec {
            ModelSpec::Main => self.entries_of(&ModelSpec::Named(MAIN_ROLE.to_string())),
            ModelSpec::GuardianDefault => {
                Err("`$guardian_default` is not a model list".to_string())
            }
            ModelSpec::Model(text) => Ok((None, self.flatten_entries(std::slice::from_ref(text))?)),
            ModelSpec::Named(name) => self
                .get(name)
                .map(|entries| (Some(name.clone()), entries.to_vec()))
                .ok_or_else(|| self.unknown(name)),
            ModelSpec::List(entries) => Ok((None, self.flatten_entries(entries)?)),
        }
    }

    /// A chain of its own for model text: a reference, a role name or a list. The session's main
    /// chain is built this way, so no role's shared chain can move it.
    pub fn chain_from_text(&self, text: &str) -> Result<ModelChain, String> {
        let spec = ModelSpec::parse(text)?;
        let (role, entries) = self.entries_of(&spec)?;
        Ok(ModelChain::new(role, entries))
    }

    /// The chain of the `main` role, as a chain of its own.
    pub fn main_chain(&self) -> Option<ModelChain> {
        self.get(MAIN_ROLE)
            .map(|entries| ModelChain::new(Some(MAIN_ROLE.to_string()), entries.to_vec()))
    }
}

fn join(entries: &[ModelRef]) -> String {
    entries
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn flatten_role(
    name: &str,
    raw: &BTreeMap<String, Vec<String>>,
    config: &Config,
    unchecked_accounts: &[&str],
    stack: &mut Vec<String>,
    done: &mut BTreeMap<String, Vec<ModelRef>>,
) -> Result<Vec<ModelRef>, ConfigError> {
    if let Some(flat) = done.get(name) {
        return Ok(flat.clone());
    }
    let bad = |message: String| ConfigError::model(format!("[models.roles] `{name}`: {message}"));
    let mut out: Vec<ModelRef> = Vec::new();
    for entry in &raw[name] {
        let spliced = if entry.contains('/') {
            let reference = ModelRef::parse(entry)
                .ok_or_else(|| bad(format!("`{entry}`: expected `<account>/<model>`")))?;
            if !unchecked_accounts.contains(&name)
                && !config.accounts.contains_key(&reference.account)
            {
                return Err(bad(format!(
                    "`{entry}` names unknown account `{}`; {}",
                    reference.account,
                    known_accounts(config)
                )));
            }
            vec![reference]
        } else {
            let target = if entry == "$main" {
                MAIN_ROLE
            } else {
                entry.as_str()
            };
            if !raw.contains_key(target) {
                return Err(bad(format!(
                    "`{entry}` is neither `<account>/<model>` nor a defined role"
                )));
            }
            if let Some(at) = stack.iter().position(|seen| seen == target) {
                let mut path = stack[at..].to_vec();
                path.push(target.to_string());
                return Err(bad(format!("reaches itself: {}", path.join(" -> "))));
            }
            stack.push(target.to_string());
            let flat = flatten_role(target, raw, config, unchecked_accounts, stack, done)?;
            stack.pop();
            flat
        };
        for reference in spliced {
            if !out.contains(&reference) {
                out.push(reference);
            }
        }
    }
    done.insert(name.to_string(), out.clone());
    Ok(out)
}

/// One preference list being served, with the entry currently in use.
#[derive(Debug)]
pub struct ModelChain {
    /// The role it serves; `None` for an inline list or a lone `<account>/<model>`.
    pub role: Option<String>,
    pub entries: Vec<ModelRef>,
    active: AtomicUsize,
}

impl ModelChain {
    pub fn new(role: Option<String>, entries: Vec<ModelRef>) -> ModelChain {
        assert!(!entries.is_empty(), "a model chain has an entry");
        ModelChain {
            role,
            entries,
            active: AtomicUsize::new(0),
        }
    }

    /// A chain of one model, with no role.
    pub fn single(reference: ModelRef) -> Arc<ModelChain> {
        Arc::new(ModelChain::new(None, vec![reference]))
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    pub fn active_ref(&self) -> &ModelRef {
        &self.entries[self.active()]
    }

    /// The order to try after `failed` failed: the current active entry first when it differs
    /// from `failed` (a concurrent caller already settled it), then the rest from the most
    /// preferred down, each at most once and excluding `failed`.
    pub fn fall_through_order(&self, failed: usize) -> Vec<usize> {
        let active = self.active();
        let mut order = Vec::with_capacity(self.entries.len());
        if active != failed && active < self.entries.len() {
            order.push(active);
        }
        for index in 0..self.entries.len() {
            if index != failed && !order.contains(&index) {
                order.push(index);
            }
        }
        order
    }

    /// Makes `index` the entry later calls start on.
    pub fn settle(&self, index: usize) {
        assert!(
            index < self.entries.len(),
            "settled on an entry the chain has"
        );
        self.active.store(index, Ordering::Release);
    }

    /// What names the list in a message: its role, or `the model list`.
    pub fn label(&self) -> String {
        self.role
            .clone()
            .unwrap_or_else(|| "the model list".to_string())
    }

    /// Runs `call` on the active entry's handle and, on an error `falls_through` accepts, on the
    /// others in fall-through order. `resolve` builds a handle for an entry; `fallback` says it
    /// is not the entry the chain started on, where `Ok(None)` skips it. An entry that fails to
    /// resolve is skipped too, and the last error is returned if nothing served. With `settle`
    /// the serving entry becomes the chain's active one.
    pub async fn run_through<H, T, E, Rf, R, Cf, C>(
        &self,
        settle: bool,
        mut resolve: R,
        mut call: C,
        falls_through: impl Fn(&E) -> bool,
    ) -> Result<Through<T>, E>
    where
        R: FnMut(usize, bool) -> Rf,
        Rf: Future<Output = Result<Option<H>, E>>,
        C: FnMut(H) -> Cf,
        Cf: Future<Output = Result<T, E>>,
    {
        let start = self.active();
        let mut order = vec![start];
        order.extend(self.fall_through_order(start));
        let mut last: Option<E> = None;
        for index in order {
            let handle = match resolve(index, index != start).await {
                Ok(Some(handle)) => handle,
                Ok(None) => continue,
                Err(e) if index == start => return Err(e),
                Err(e) => {
                    last = Some(e);
                    continue;
                }
            };
            match call(handle).await {
                Ok(value) => {
                    if settle {
                        self.settle(index);
                    }
                    return Ok(Through {
                        value,
                        index,
                        from: (index != start).then_some(start),
                    });
                }
                Err(e) if falls_through(&e) => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last.expect("the first entry either served or returned its error"))
    }
}

/// The entries a list has already failed on while one request is being served, so falling
/// through ends once each has been tried instead of cycling between two spent accounts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tried(Vec<usize>);

impl Tried {
    pub fn note(&mut self, index: usize) {
        if !self.0.contains(&index) {
            self.0.push(index);
        }
    }

    /// The order to try after `failed` failed, without the entries already tried.
    pub fn order(&mut self, chain: &ModelChain, failed: usize) -> Vec<usize> {
        self.note(failed);
        chain
            .fall_through_order(failed)
            .into_iter()
            .filter(|index| !self.0.contains(index))
            .collect()
    }
}

/// The warning for a list that moved to its next entry. `subject` is whose model it was: the
/// list's role for the main conversation, an agent for a subagent.
pub fn fall_through_notice(subject: &str, from: &ModelRef, to: &ModelRef, class: &str) -> String {
    format!("{subject}: {from} is out of quota ({class}); continuing on {to}")
}

/// A one-shot job that ran through a chain.
#[derive(Debug)]
pub struct Through<T> {
    pub value: T,
    /// The entry that served.
    pub index: usize,
    /// The entry the chain started on, when another one served.
    pub from: Option<usize>,
}

/// The chains of a session's roles and inline lists, so every caller on one role shares the
/// entry that last worked.
#[derive(Debug, Default)]
pub struct RoleChains {
    chains: Mutex<BTreeMap<String, Arc<ModelChain>>>,
}

impl RoleChains {
    /// The chain a spec stands for: the shared one of a role or an inline list, and a fresh
    /// one-entry chain for a lone reference. `$main` and `$guardian_default` are the session's
    /// own, which the caller serves.
    pub fn chain_for(&self, roles: &Roles, spec: &ModelSpec) -> Result<Arc<ModelChain>, String> {
        if matches!(spec, ModelSpec::Main | ModelSpec::GuardianDefault) {
            return Err(format!("`{}` is not a role", spec.as_text()));
        }
        let (role, entries) = roles.entries_of(spec)?;
        let key = match (&role, spec) {
            (Some(name), _) => name.clone(),
            (None, ModelSpec::List(_)) => format!("list:{}", join(&entries)),
            (None, _) => return Ok(ModelChain::single(entries[0].clone())),
        };
        let mut chains = lock(&self.chains, "role chains");
        Ok(chains
            .entry(key)
            .or_insert_with(|| Arc::new(ModelChain::new(role, entries)))
            .clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Account, OneOrMany};

    fn config(roles: &[(&str, &[&str])]) -> Config {
        let mut config = Config::defaults(std::env::temp_dir());
        for account in ["a", "b", "c"] {
            let parsed: Account = toml::from_str("kind = \"openai-compatible\"").unwrap();
            config.accounts.insert(account.to_string(), parsed);
        }
        for (name, list) in roles {
            config.roles.insert(
                name.to_string(),
                list.iter().map(|e| e.to_string()).collect(),
            );
        }
        config
    }

    fn refs(roles: &Roles, name: &str) -> Vec<String> {
        roles
            .get(name)
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn a_role_names_its_entries_and_splices_other_roles_with_the_first_copy_kept() {
        let roles = Roles::from_config(&config(&[
            ("sonnet", &["a/s5", "a/s4"]),
            ("coding", &["sonnet", "b/kimi", "a/s4"]),
        ]))
        .unwrap();
        assert_eq!(refs(&roles, "coding"), ["a/s5", "a/s4", "b/kimi"]);
        assert_eq!(
            roles.describe(),
            "- `coding`: a/s5, a/s4, b/kimi\n- `sonnet`: a/s5, a/s4"
        );
    }

    #[test]
    fn model_key_folds_into_main_and_main_and_dollar_main_splice_it() {
        let mut config = config(&[("thinking", &["$main", "b/x"]), ("deep", &["main"])]);
        config.model = Some(OneOrMany::Many(vec!["a/one".into(), "c/two".into()]));
        config.main_source = MainSource::ModelKeyList;
        let roles = Roles::from_config(&config).unwrap();
        assert_eq!(refs(&roles, "main"), ["a/one", "c/two"]);
        assert_eq!(refs(&roles, "thinking"), ["a/one", "c/two", "b/x"]);
        assert_eq!(refs(&roles, "deep"), ["a/one", "c/two"]);
        assert_eq!(roles.main_chain().unwrap().entries.len(), 2);
    }

    #[test]
    fn a_cycle_is_refused_with_its_path() {
        for roles in [
            &[("a", &["b"][..]), ("b", &["a"][..])][..],
            &[("a", &["b"]), ("b", &["c"]), ("c", &["a/x", "a"])],
            &[("a", &["a"])],
        ] {
            let error = Roles::from_config(&config(roles)).unwrap_err().to_string();
            assert!(error.contains("reaches itself"), "{error}");
        }
        let error = Roles::from_config(&config(&[("a", &["b"]), ("b", &["a"])]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("a -> b -> a"), "{error}");
        let mut config = config(&[]);
        config.model = Some(OneOrMany::One("$main".into()));
        config.main_source = MainSource::ModelKeyString;
        assert!(Roles::from_config(&config).is_err());
    }

    #[test]
    fn an_undefined_role_an_unknown_account_and_a_bad_name_are_refused() {
        let error = Roles::from_config(&config(&[("a", &["ghost"])]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("`ghost`") && error.contains("defined role"),
            "{error}"
        );
        let error = Roles::from_config(&config(&[("a", &["nope/x"])]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown account `nope`"), "{error}");
        for name in ["inherit", "$x", "a/b", ""] {
            assert!(
                Roles::from_config(&config(&[(name, &["a/x"])])).is_err(),
                "{name}"
            );
        }
        assert!(Roles::from_config(&config(&[("empty", &[])])).is_err());
    }

    #[test]
    fn an_unknown_account_in_model_key_is_left_to_resolution() {
        let mut config = config(&[]);
        config.model = Some(OneOrMany::One("ghost/x".into()));
        config.main_source = MainSource::ModelKeyString;
        let roles = Roles::from_config(&config).unwrap();
        assert_eq!(refs(&roles, "main"), ["ghost/x"]);
    }

    #[test]
    fn text_becomes_a_chain_of_its_own() {
        let roles = Roles::from_config(&config(&[("coding", &["a/x", "b/y"])])).unwrap();
        let chain = roles.chain_from_text("coding").unwrap();
        assert_eq!(chain.role.as_deref(), Some("coding"));
        assert_eq!(chain.entries.len(), 2);
        let lone = roles.chain_from_text("c/z").unwrap();
        assert_eq!((lone.role.clone(), lone.entries.len()), (None, 1));
        let listed = roles.chain_from_text("c/z,coding").unwrap();
        assert_eq!(listed.entries.len(), 3);
        assert!(roles.chain_from_text("ghost").is_err());
    }

    #[test]
    fn role_chains_share_one_active_entry_per_role_and_never_the_session_main() {
        let roles = Roles::from_config(&config(&[("coding", &["a/x", "b/y"])])).unwrap();
        let chains = RoleChains::default();
        let named = ModelSpec::Named("coding".into());
        let first = chains.chain_for(&roles, &named).unwrap();
        let second = chains.chain_for(&roles, &named).unwrap();
        first.settle(1);
        assert_eq!(second.active(), 1);
        let list = ModelSpec::List(vec!["a/x".into(), "b/y".into()]);
        assert_eq!(chains.chain_for(&roles, &list).unwrap().active(), 0);
        assert!(chains.chain_for(&roles, &ModelSpec::Main).is_err());
        let lone = chains
            .chain_for(&roles, &ModelSpec::Model("c/z".into()))
            .unwrap();
        assert_eq!(lone.role, None);
        let session = roles.chain_from_text("coding").unwrap();
        assert_eq!(session.active(), 0);
    }

    fn chain(n: usize) -> ModelChain {
        ModelChain::new(
            Some("r".into()),
            (0..n)
                .map(|i| ModelRef {
                    account: "a".into(),
                    model: format!("m{i}"),
                })
                .collect(),
        )
    }

    #[test]
    fn fall_through_order_starts_at_the_active_entry_when_another_caller_settled_it() {
        let chain = chain(4);
        assert_eq!(chain.fall_through_order(0), [1, 2, 3]);
        assert_eq!(chain.fall_through_order(2), [0, 1, 3]);
        chain.settle(2);
        assert_eq!(chain.fall_through_order(2), [0, 1, 3]);
        assert_eq!(chain.fall_through_order(0), [2, 1, 3]);
        assert_eq!(chain.fall_through_order(3), [2, 0, 1]);
        chain.settle(3);
        assert_eq!(chain.fall_through_order(3), [0, 1, 2]);
        assert_eq!(chain.active_ref().model, "m3");
        assert!(self::chain(1).fall_through_order(0).is_empty());
    }

    #[test]
    fn a_request_tries_each_entry_once() {
        let chain = chain(3);
        let mut tried = Tried::default();
        assert_eq!(tried.order(&chain, 0), [1, 2]);
        chain.settle(1);
        assert_eq!(tried.order(&chain, 1), [2]);
        chain.settle(2);
        assert!(tried.order(&chain, 2).is_empty());
        assert!(Tried::default().order(&self::chain(1), 0).is_empty());
    }

    #[test]
    fn settle_is_shared_across_clones_of_the_arc() {
        let chain = Arc::new(chain(3));
        let other = chain.clone();
        chain.settle(2);
        assert_eq!(other.active(), 2);
    }

    #[tokio::test]
    async fn run_through_stops_at_the_first_success_and_settles_it() {
        let chain = chain(3);
        let mut called = Vec::new();
        let served = chain
            .run_through(
                true,
                |index, _| async move { Ok::<_, String>(Some(index)) },
                |index| {
                    called.push(index);
                    async move {
                        if index == 0 {
                            Err("quota".to_string())
                        } else {
                            Ok(index * 10)
                        }
                    }
                },
                |e: &String| e == "quota",
            )
            .await
            .unwrap();
        assert_eq!((served.value, served.index, served.from), (10, 1, Some(0)));
        assert_eq!(called, [0, 1]);
        assert_eq!(chain.active(), 1);
    }

    #[tokio::test]
    async fn run_through_stops_on_an_error_that_does_not_fall_through() {
        let chain = chain(3);
        let mut calls = 0;
        let error = chain
            .run_through(
                true,
                |index, _| async move { Ok::<_, String>(Some(index)) },
                |_| {
                    calls += 1;
                    async { Err::<(), _>("bad request".to_string()) }
                },
                |e: &String| e == "quota",
            )
            .await
            .unwrap_err();
        assert_eq!(error, "bad request");
        assert_eq!(calls, 1);
        assert_eq!(chain.active(), 0);
    }

    #[tokio::test]
    async fn run_through_skips_entries_that_do_not_resolve_and_returns_the_last_error_when_spent() {
        let chain = chain(3);
        let result = chain
            .run_through(
                false,
                |index, fallback| async move {
                    match index {
                        1 => Err("no credential".to_string()),
                        2 if fallback => Ok(None),
                        _ => Ok(Some(index)),
                    }
                },
                |_| async { Err::<(), _>("quota".to_string()) },
                |e: &String| e == "quota",
            )
            .await;
        assert_eq!(result.unwrap_err(), "no credential");
        assert_eq!(chain.active(), 0);
    }
}
