//! The optional last step of `fontaine auto-mode setup` and `/auto-mode-setup`: offering the
//! guardian composition's `screen` preset, on a local model with a threshold measured for that exact
//! model or on the hosted model with the preset's defaults. Both surfaces share this module so they
//! detect, write and explain the same way.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use toml_edit::{Table, value};

use crate::config::{
    Config, ConfigError, DEFAULT_SCREEN_TIMEOUT_MS, EnvLookup, GuardianComposition, backend_of,
    check_composition, screen_key, systemone_endpoint,
};
use crate::credentials;
use crate::models::AccountListing;

/// A threshold pair fitted to one model. The fit holds for that exact Ollama tag, quantization,
/// prompt and question, so a near tag never matches.
#[derive(Debug, PartialEq)]
pub struct Benchmark {
    pub tag: &'static str,
    pub stop_threshold: f64,
    pub allow_threshold: f64,
    pub evidence: &'static str,
}

pub const BENCHMARKS: &[Benchmark] = &[Benchmark {
    tag: "qwen3:4b-instruct-2507-q4_K_M",
    stop_threshold: 0.0000599,
    allow_threshold: 0.00000156,
    evidence: "stop 0.0000599 stops the riskiest 10% of the allowed cases in the synthetic calibration corpus; \
allow 0.00000156 is the zero-miss threshold on that experiment (13 of 59 should-allow cases skipped the guardian, \
none that must not be cleared) but let 1 of 15 live allows through that the dataset expects escalated; \
measured 2026-10-24 on dataset 5a79aa741c27 (see docs/evals.md)",
}];

/// The hosted model the screen preset is offered with, on an OpenRouter account.
pub const JEV_MODEL: &str = "typesafe/jev-1.14";

/// An account the screen can be offered on.
#[derive(Clone, Debug)]
pub enum Candidate {
    /// An Ollama account that holds a benchmarked model.
    Ollama {
        account: String,
        benchmark: &'static Benchmark,
    },
    /// An OpenRouter account whose key resolves.
    Jev { account: String },
}

impl Candidate {
    pub fn account(&self) -> &str {
        match self {
            Candidate::Ollama { account, .. } | Candidate::Jev { account } => account,
        }
    }

    /// The `<account>/<model>` the screen would read with.
    pub fn model(&self) -> String {
        match self {
            Candidate::Ollama { account, benchmark } => format!("{account}/{}", benchmark.tag),
            Candidate::Jev { account } => format!("{account}/{JEV_MODEL}"),
        }
    }

    /// Whether the offer on this candidate asks safe or fast.
    pub fn asks_posture(&self) -> bool {
        matches!(self, Candidate::Ollama { .. })
    }
}

/// The one choice the user makes on an Ollama account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Posture {
    /// The screen can only escalate or deny; the LLM guardian reviews every call it did not stop.
    Safe,
    /// The screen allows what it is confident about; the LLM guardian decides the rest.
    Fast,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Offer {
    /// A local model with a threshold measured for its exact tag, in the posture chosen.
    Ollama {
        account: String,
        benchmark: &'static Benchmark,
        posture: Posture,
    },
    /// The hosted screen with the preset's default thresholds and its allow path on.
    Jev { account: String },
}

impl Posture {
    pub fn describe(self) -> &'static str {
        match self {
            Posture::Safe => {
                "safe: the screen can only stop a call (escalate it, or deny it when nobody can answer); \
it never allows, so the LLM guardian still reviews every call the screen did not stop"
            }
            Posture::Fast => {
                "fast: the screen allows the calls it is confident about; every call it does not clear goes to \
the LLM guardian, which decides it"
            }
        }
    }
}

impl Offer {
    /// The offer on `candidate`; `posture` is what an Ollama candidate asked, and is ignored on a
    /// hosted one.
    pub fn on(candidate: &Candidate, posture: Option<Posture>) -> Option<Offer> {
        match candidate {
            Candidate::Ollama { account, benchmark } => Some(Offer::Ollama {
                account: account.clone(),
                benchmark,
                posture: posture?,
            }),
            Candidate::Jev { account } => Some(Offer::Jev {
                account: account.clone(),
            }),
        }
    }

    pub fn account(&self) -> &str {
        match self {
            Offer::Ollama { account, .. } | Offer::Jev { account } => account,
        }
    }

    pub fn model(&self) -> String {
        match self {
            Offer::Ollama {
                account, benchmark, ..
            } => format!("{account}/{}", benchmark.tag),
            Offer::Jev { account } => format!("{account}/{JEV_MODEL}"),
        }
    }

    /// The posture chosen, for an offer that has one.
    pub fn posture(&self) -> Option<Posture> {
        match self {
            Offer::Ollama { posture, .. } => Some(*posture),
            Offer::Jev { .. } => None,
        }
    }

    /// What the confirmation and the report call the offer.
    pub fn name(&self) -> &'static str {
        match self {
            Offer::Ollama {
                posture: Posture::Safe,
                ..
            } => "safe",
            Offer::Ollama {
                posture: Posture::Fast,
                ..
            } => "fast",
            Offer::Jev { .. } => "hosted",
        }
    }

    /// The threshold written, for an offer that writes one: a hosted offer writes none, so a new
    /// default reaches it without a config rewrite.
    pub fn threshold(&self) -> Option<f64> {
        match self {
            Offer::Ollama {
                benchmark, posture, ..
            } => Some(match posture {
                Posture::Safe => benchmark.stop_threshold,
                Posture::Fast => benchmark.allow_threshold,
            }),
            Offer::Jev { .. } => None,
        }
    }

    fn threshold_key(&self) -> Option<&'static str> {
        match self {
            Offer::Ollama {
                posture: Posture::Safe,
                ..
            } => Some("stop_threshold"),
            Offer::Ollama {
                posture: Posture::Fast,
                ..
            } => Some("allow_threshold"),
            Offer::Jev { .. } => None,
        }
    }

    /// What is about to be written, for the confirmation.
    pub fn proposal(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "[guardian.composition] preset = \"screen\", model = \"{}\", shadow = false",
            self.model()
        )];
        match self {
            Offer::Ollama { benchmark, .. } => {
                lines.push(format!(
                    "[guardian.composition.params] {} = {}",
                    self.threshold_key()
                        .expect("an Ollama offer writes a threshold"),
                    self.threshold()
                        .expect("an Ollama offer writes a threshold")
                ));
                lines.push(format!("benchmark: {}", benchmark.evidence));
            }
            Offer::Jev { .. } => {
                use fontaine_guardian::composition::{JEV_ALLOW_THRESHOLD, JEV_STOP_THRESHOLD};
                lines.push(
                    "no threshold is written: the preset's defaults apply, so a recalibration reaches you without a config change"
                        .to_owned(),
                );
                lines.push(format!(
                    "data leaves this machine: for every call the guardian judges, the screen sends the task intent, recent user messages, the tool name and arguments, and project facts to OpenRouter and Typesafe ({JEV_MODEL})"
                ));
                lines.push(format!(
                    "the allow path is on: a call scored below {JEV_ALLOW_THRESHOLD} risk skips the LLM guardian, and one scored {JEV_STOP_THRESHOLD} or above is escalated"
                ));
                lines.push(
                    "to check real traffic before it acts, set `shadow = true` under [guardian.composition]; to turn the allow path off, set `allow = false` under [guardian.composition.params]"
                        .to_owned(),
                );
            }
        }
        lines
    }
}

/// Every reachable Ollama account that lists a benchmarked tag exactly. An unreachable account
/// and a near tag give nothing.
pub fn detect(listings: &[AccountListing]) -> Vec<Candidate> {
    let mut found = Vec::new();
    for listing in listings {
        if listing.kind.as_str() != "ollama" {
            continue;
        }
        let Ok(models) = &listing.models else {
            continue;
        };
        for benchmark in BENCHMARKS {
            if models.iter().any(|m| m == benchmark.tag) {
                found.push(Candidate::Ollama {
                    account: listing.account.clone(),
                    benchmark,
                });
            }
        }
    }
    found
}

/// The accounts worth listing for [`detect`]: only an Ollama account can hold a benchmarked model,
/// so no other provider is contacted.
pub fn ollama_accounts(config: &Config) -> Vec<String> {
    config
        .accounts
        .iter()
        .filter(|(_, account)| account.kind.as_str() == "ollama")
        .map(|(name, _)| name.clone())
        .collect()
}

/// Every OpenRouter account whose key resolves and whose address a key may be sent to. Judged by
/// the credential alone: the hosted model is named by the offer, so nothing is listed and no
/// provider is contacted.
pub fn detect_hosted(
    config: &Config,
    env: EnvLookup<'_>,
    data_dir: &Path,
) -> Result<Vec<Candidate>, ConfigError> {
    let stored = credentials::Credentials::load(data_dir)?;
    Ok(config
        .accounts
        .iter()
        .filter(|(_, account)| {
            backend_of(account.kind) == Some(fontaine_guardian::BackendKind::SystemOne)
        })
        .filter(|(name, account)| {
            screen_key(name, account, env, &stored).is_ok()
                && systemone_endpoint(account)
                    .is_ok_and(|url| fontaine_guardian::systemone::key_may_travel(&url))
        })
        .map(|(name, _)| Candidate::Jev {
            account: name.clone(),
        })
        .collect())
}

/// Everything setup can offer, plus the notes to show alongside: a hosted screen that could not
/// be checked, or the hint to add an OpenRouter account when detection found none.
pub fn offer_candidates(
    config: &Config,
    env: EnvLookup<'_>,
    data_dir: &Path,
    listings: &[AccountListing],
) -> (Vec<Candidate>, Vec<String>) {
    let mut notes = Vec::new();
    let mut found = match detect_hosted(config, env, data_dir) {
        Ok(hosted) => {
            if hosted.is_empty() {
                notes.push(hosted_hint());
            }
            hosted
        }
        Err(e) => {
            notes.push(format!(
                "guardian composition: the hosted screen was not checked: {}",
                fontaine_tools::error_chain::chain(&e)
            ));
            Vec::new()
        }
    };
    found.extend(detect(listings));
    (found, notes)
}

/// Whether a composition is already configured; setup never replaces one.
pub fn existing(config: &Config) -> bool {
    config.guardian.composition.is_some()
}

/// The line for a run that has nothing to offer on Ollama.
pub fn nothing_to_offer() -> String {
    let tags: Vec<&str> = BENCHMARKS.iter().map(|b| b.tag).collect();
    format!(
        "guardian composition: not offered; no reachable Ollama account holds a benchmarked model (ollama pull {})",
        tags.join(", ")
    )
}

/// The line for a run with no OpenRouter account to offer the hosted screen on.
pub fn hosted_hint() -> String {
    format!(
        "guardian composition: the hosted screen ({JEV_MODEL}) needs an OpenRouter account; add one with `/login` or `api_key_env` and run setup again"
    )
}

/// Writes the offered composition to the user settings file in one edit. The section is first
/// run through the same check a session start applies, so one that would stop startup is
/// refused. Nothing but the user file is touched.
pub fn write(user_settings: &Path, config: &Config, offer: &Offer) -> io::Result<()> {
    let params: BTreeMap<String, serde_json::Value> = offer
        .threshold_key()
        .zip(offer.threshold())
        .map(|(key, threshold)| (key.to_owned(), threshold.into()))
        .into_iter()
        .collect();
    let mut checked = config.clone();
    checked.guardian.composition = Some(GuardianComposition {
        preset: Some(fontaine_guardian::composition::SCREEN.to_owned()),
        script: None,
        script_source: None,
        model: Some(offer.model()),
        shadow: false,
        timeout_ms: DEFAULT_SCREEN_TIMEOUT_MS,
        params,
        reads: BTreeMap::new(),
    });
    let config_dir = user_settings.parent().unwrap_or_else(|| Path::new("."));
    check_composition(&mut checked, user_settings, config_dir)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let mut section = Table::new();
    section.insert("preset", value(fontaine_guardian::composition::SCREEN));
    section.insert("model", value(offer.model()));
    section.insert("shadow", value(false));
    if let Some((key, threshold)) = offer.threshold_key().zip(offer.threshold()) {
        let mut params = Table::new();
        params.insert(key, value(threshold));
        section.insert("params", toml_edit::Item::Table(params));
    }
    crate::user_config::set_table(user_settings, &["guardian", "composition"], section)
}

/// What to do after the write: the value is a fit on a small dataset, not a guarantee.
pub fn calibration_note(offer: &Offer) -> Vec<String> {
    let model = offer.model();
    match offer {
        Offer::Ollama { posture, .. } => {
            let (risk, caveat) = match posture {
                Posture::Safe => (
                    "screen_risk",
                    "safe may also stop some calls that would otherwise be allowed",
                ),
                Posture::Fast => (
                    "screen_allow_risk",
                    "the fast threshold let one call through live that the dataset expects denied or escalated",
                ),
            };
            vec![
                "the threshold is a fit on a small dataset, not zero-miss on every live run".to_owned(),
                format!("note: {caveat}"),
                format!(
                    "score it on your own traffic: fontaine-evals --screen live:{model}, or read the {risk} values the audit log records"
                ),
                "set `shadow = true` under [guardian.composition] to log scores without acting while you tune".to_owned(),
                "the composition applies from the next session".to_owned(),
            ]
        }
        Offer::Jev { .. } => vec![
            "the default thresholds are a fit on a synthetic corpus with a margin for the model's nondeterminism, not zero-miss on every live run".to_owned(),
            "note: known leak classes are `.env` reads, session facts and `kubectl get ... secrets`".to_owned(),
            format!(
                "score it on your own traffic: fontaine-evals --screen live:{model}, or read the screen_risk and screen_allow_risk values the audit log records"
            ),
            "set `shadow = true` under [guardian.composition] to log scores without acting while you tune; `allow = false` under [guardian.composition.params] turns the allow path off".to_owned(),
            "the composition applies from the next session".to_owned(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AccountKind, CliRules, LoadParams, load};

    fn listing(account: &str, kind: &str, models: Result<Vec<&str>, &str>) -> AccountListing {
        AccountListing {
            account: account.to_owned(),
            kind: AccountKind::named(kind),
            models: models
                .map(|m| m.into_iter().map(str::to_owned).collect())
                .map_err(str::to_owned),
        }
    }

    const TAG: &str = BENCHMARKS[0].tag;

    #[test]
    fn detect_finds_only_exact_tags_on_reachable_ollama_accounts() {
        assert!(detect(&[]).is_empty());
        let none = [listing("local", "ollama", Ok(vec!["llama3"]))];
        assert!(detect(&none).is_empty());
        let near = [listing(
            "local",
            "ollama",
            Ok(vec!["qwen3:4b-instruct-2507-q8_0", "qwen3:4b"]),
        )];
        assert!(detect(&near).is_empty());
        let down = [listing("local", "ollama", Err("connection refused"))];
        assert!(detect(&down).is_empty());
        let other_kind = [listing("cloud", "openai-compatible", Ok(vec![TAG]))];
        assert!(detect(&other_kind).is_empty());

        let several = [
            listing("a", "ollama", Ok(vec![TAG, "x"])),
            listing("b", "ollama", Err("down")),
            listing("c", "ollama", Ok(vec![TAG])),
        ];
        let found = detect(&several);
        let names: Vec<&str> = found.iter().map(|c| c.account()).collect();
        assert_eq!(names, ["a", "c"]);
    }

    struct World {
        _dir: tempfile::TempDir,
        launch: std::path::PathBuf,
        config_dir: std::path::PathBuf,
        data: std::path::PathBuf,
    }

    impl World {
        fn new(user: &str) -> World {
            let dir = tempfile::tempdir().unwrap();
            let world = World {
                launch: dir.path().join("work"),
                config_dir: dir.path().join("config"),
                data: dir.path().join("data"),
                _dir: dir,
            };
            std::fs::create_dir_all(&world.launch).unwrap();
            std::fs::create_dir_all(&world.config_dir).unwrap();
            std::fs::write(world.user_file(), user).unwrap();
            world
        }

        fn user_file(&self) -> std::path::PathBuf {
            self.config_dir.join(crate::config::USER_FILE)
        }

        fn load(&self) -> Result<Config, crate::config::ConfigError> {
            self.load_with(&|_: &str| None)
        }

        fn load_with(
            &self,
            env: crate::config::EnvLookup<'_>,
        ) -> Result<Config, crate::config::ConfigError> {
            load(&LoadParams {
                launch_dir: &self.launch,
                config_dir: &self.config_dir,
                data_dir: &self.data,
                settings_file: None,
                cli_rules: CliRules {
                    allow: &[],
                    deny: &[],
                },
                env,
            })
        }
    }

    const USER: &str = "# mine\n[accounts.local]\nkind = \"ollama\"\n\n[tui]\ntheme = \"dark\"\n";

    fn offer(posture: Posture) -> Offer {
        Offer::Ollama {
            account: "local".to_owned(),
            benchmark: &BENCHMARKS[0],
            posture,
        }
    }

    const HOSTED: &str = "[accounts.or]\nkind = \"openrouter\"\napi_key_env = \"OR_KEY\"\n";

    fn hosted() -> Offer {
        Offer::Jev {
            account: "or".to_owned(),
        }
    }

    fn with_key(name: &str) -> Option<std::ffi::OsString> {
        (name == "OR_KEY").then(|| "sk-or-test".into())
    }

    #[test]
    fn every_benchmark_threshold_is_valid_for_the_preset() {
        use fontaine_guardian::composition::{Param, check_preset_params};
        for benchmark in BENCHMARKS {
            for (key, t) in [
                ("stop_threshold", benchmark.stop_threshold),
                ("allow_threshold", benchmark.allow_threshold),
            ] {
                let params = BTreeMap::from([(key.to_owned(), Param::Number(t))]);
                check_preset_params(
                    "screen",
                    &params,
                    Some(fontaine_guardian::BackendKind::Ollama),
                )
                .unwrap_or_else(|e| panic!("{} {key}: {}", benchmark.tag, e.message));
            }
        }
    }

    #[test]
    fn each_posture_writes_a_loadable_section_with_only_its_threshold() {
        for posture in [Posture::Safe, Posture::Fast] {
            let world = World::new(USER);
            let config = world.load().unwrap();
            assert!(!existing(&config));
            let offer = offer(posture);
            write(&world.user_file(), &config, &offer).unwrap();

            let text = std::fs::read_to_string(world.user_file()).unwrap();
            assert!(text.starts_with("# mine\n"), "{text}");
            assert!(text.contains("theme = \"dark\""), "{text}");

            let loaded = world.load().unwrap();
            assert!(existing(&loaded));
            let composition = loaded.guardian.composition.unwrap();
            assert_eq!(composition.preset.as_deref(), Some("screen"));
            assert_eq!(composition.model.as_deref(), Some(&*offer.model()));
            assert!(!composition.shadow);
            let (set, unset) = match posture {
                Posture::Safe => ("stop_threshold", "allow_threshold"),
                Posture::Fast => ("allow_threshold", "stop_threshold"),
            };
            assert_eq!(composition.params.len(), 1, "{:?}", composition.params);
            assert_eq!(composition.params[set].as_f64(), offer.threshold());
            assert!(!composition.params.contains_key(unset));
        }
    }

    #[test]
    fn a_section_that_would_not_load_is_refused_and_nothing_is_written() {
        let world =
            World::new("[accounts.cloud]\nkind = \"openai-compatible\"\nbase_url = \"http://x\"\n");
        let config = world.load().unwrap();
        let before = std::fs::read_to_string(world.user_file()).unwrap();
        let on = |account: &str| Offer::Ollama {
            account: account.to_owned(),
            benchmark: &BENCHMARKS[0],
            posture: Posture::Safe,
        };
        let err = write(&world.user_file(), &config, &on("cloud")).unwrap_err();
        assert!(err.to_string().contains("ollama"), "{err}");
        write(&world.user_file(), &config, &on("absent")).unwrap_err();
        assert_eq!(std::fs::read_to_string(world.user_file()).unwrap(), before);
    }

    #[test]
    fn a_write_touches_the_user_file_only() {
        let world = World::new(USER);
        let config = world.load().unwrap();
        write(&world.user_file(), &config, &offer(Posture::Fast)).unwrap();
        assert!(!world.launch.join(crate::config::PROJECT_DIR).exists());
    }

    #[test]
    fn the_note_names_the_posture_s_audit_value() {
        assert!(
            calibration_note(&offer(Posture::Safe))
                .join("\n")
                .contains("screen_risk")
        );
        let fast = calibration_note(&offer(Posture::Fast)).join("\n");
        assert!(fast.contains("screen_allow_risk"), "{fast}");
        assert!(
            fast.contains("live:local/qwen3:4b-instruct-2507-q4_K_M"),
            "{fast}"
        );
    }

    #[test]
    fn the_hosted_offer_is_made_only_on_an_openrouter_account_with_a_key() {
        let world = World::new(&format!("{USER}{HOSTED}"));
        let config = world.load().unwrap();
        let none = detect_hosted(&config, &|_: &str| None, &world.data).unwrap();
        assert!(none.is_empty());
        let found = detect_hosted(&config, &with_key, &world.data).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].account(), "or");
        assert_eq!(found[0].model(), format!("or/{JEV_MODEL}"));
        assert!(!found[0].asks_posture());

        let no_account = World::new(USER);
        let config = no_account.load().unwrap();
        assert!(
            detect_hosted(&config, &with_key, &no_account.data)
                .unwrap()
                .is_empty()
        );
        assert!(hosted_hint().contains("/login"));
        assert!(hosted_hint().contains("api_key_env"));
    }

    #[test]
    fn a_key_that_may_not_travel_makes_no_hosted_offer() {
        let plain = "[accounts.or]\nkind = \"openrouter\"\napi_key_env = \"OR_KEY\"\nbase_url = \"http://example.com/api/v1\"\n";
        let world = World::new(plain);
        let config = world.load().unwrap();
        assert!(
            detect_hosted(&config, &with_key, &world.data)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_hosted_proposal_names_the_egress_the_allow_path_and_the_ways_out() {
        let text = hosted().proposal().join("\n");
        assert!(text.contains("data leaves this machine"), "{text}");
        assert!(text.contains("OpenRouter"), "{text}");
        assert!(text.contains("the allow path is on"), "{text}");
        assert!(text.contains("allow = false"), "{text}");
        assert!(text.contains("shadow = true"), "{text}");
        assert!(text.contains("shadow = false"), "{text}");
        assert_eq!(hosted().threshold(), None);
        assert_eq!(hosted().posture(), None);
    }

    #[test]
    fn the_hosted_offer_writes_exactly_the_preset_the_model_and_shadow_off() {
        let world = World::new(&format!("{USER}{HOSTED}"));
        let config = world.load().unwrap();
        write(&world.user_file(), &config, &hosted()).unwrap();

        let text = std::fs::read_to_string(world.user_file()).unwrap();
        assert!(text.starts_with("# mine\n"), "{text}");
        assert!(!text.contains("threshold"), "{text}");
        assert!(!text.contains("params"), "{text}");
        assert!(!text.contains("[accounts.new"), "{text}");

        let loaded = world.load_with(&with_key).unwrap();
        let composition = loaded.guardian.composition.unwrap();
        assert_eq!(composition.preset.as_deref(), Some("screen"));
        assert_eq!(composition.model.as_deref(), Some("or/typesafe/jev-1.14"));
        assert!(!composition.shadow);
        assert!(composition.params.is_empty());
        assert!(composition.reads.is_empty());
    }

    #[test]
    fn declining_the_hosted_offer_writes_nothing_and_a_missing_account_is_refused() {
        let world = World::new(USER);
        let config = world.load().unwrap();
        let before = std::fs::read_to_string(world.user_file()).unwrap();
        let err = write(&world.user_file(), &config, &hosted()).unwrap_err();
        assert!(err.to_string().contains("not configured"), "{err}");
        assert_eq!(std::fs::read_to_string(world.user_file()).unwrap(), before);
    }

    #[test]
    fn the_hosted_note_points_at_the_audit_scores_and_the_ways_out() {
        let note = calibration_note(&hosted()).join("\n");
        assert!(note.contains("screen_allow_risk"), "{note}");
        assert!(note.contains("live:or/typesafe/jev-1.14"), "{note}");
        assert!(note.contains("allow = false"), "{note}");
    }
}
