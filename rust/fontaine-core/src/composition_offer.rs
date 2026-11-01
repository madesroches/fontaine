//! The optional last step of `fontaine auto-mode setup` and `/auto-mode-setup`: offering the
//! guardian composition's `screen` preset with a threshold that was measured for the exact model
//! it is paired with. Both surfaces share this module so they detect, write and explain the same
//! way.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use toml_edit::{Table, value};

use crate::config::{Config, DEFAULT_SCREEN_TIMEOUT_MS, GuardianComposition, check_composition};
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

/// An Ollama account that holds a benchmarked model.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub account: String,
    pub benchmark: &'static Benchmark,
}

/// The one choice the user makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Posture {
    /// The screen can only escalate or deny; the LLM guardian reviews every call it did not stop.
    Safe,
    /// The screen allows what it is confident about; the LLM guardian decides the rest.
    Fast,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub account: String,
    pub benchmark: &'static Benchmark,
    pub posture: Posture,
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
    pub fn model(&self) -> String {
        format!("{}/{}", self.account, self.benchmark.tag)
    }

    pub fn threshold(&self) -> f64 {
        match self.posture {
            Posture::Safe => self.benchmark.stop_threshold,
            Posture::Fast => self.benchmark.allow_threshold,
        }
    }

    fn threshold_key(&self) -> &'static str {
        match self.posture {
            Posture::Safe => "stop_threshold",
            Posture::Fast => "allow_threshold",
        }
    }

    /// What is about to be written, for the confirmation.
    pub fn proposal(&self) -> Vec<String> {
        vec![
            format!(
                "[guardian.composition] preset = \"screen\", model = \"{}\", shadow = false",
                self.model()
            ),
            format!(
                "[guardian.composition.params] {} = {}",
                self.threshold_key(),
                self.threshold()
            ),
            format!("benchmark: {}", self.benchmark.evidence),
        ]
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
                found.push(Candidate {
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

/// Whether a composition is already configured; setup never replaces one.
pub fn existing(config: &Config) -> bool {
    config.guardian.composition.is_some()
}

/// The line for a run that has nothing to offer.
pub fn nothing_to_offer() -> String {
    let tags: Vec<&str> = BENCHMARKS.iter().map(|b| b.tag).collect();
    format!(
        "guardian composition: not offered; no reachable Ollama account holds a benchmarked model (ollama pull {})",
        tags.join(", ")
    )
}

/// Writes the offered composition to the user settings file in one edit. The section is first
/// run through the same check a session start applies, so one that would stop startup is
/// refused. Nothing but the user file is touched.
pub fn write(user_settings: &Path, config: &Config, offer: &Offer) -> io::Result<()> {
    let params = BTreeMap::from([(offer.threshold_key().to_owned(), offer.threshold().into())]);
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
    let mut params = Table::new();
    params.insert(offer.threshold_key(), value(offer.threshold()));
    section.insert("params", toml_edit::Item::Table(params));
    crate::user_config::set_table(user_settings, &["guardian", "composition"], section)
}

/// What to do after the write: the value is a fit on a small dataset, not a guarantee.
pub fn calibration_note(offer: &Offer) -> Vec<String> {
    let (risk, caveat) = match offer.posture {
        Posture::Safe => (
            "screen_risk",
            "safe may also stop some calls that would otherwise be allowed",
        ),
        Posture::Fast => (
            "screen_allow_risk",
            "the fast threshold let one call through live that the dataset expects denied or escalated",
        ),
    };
    let model = offer.model();
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
        let names: Vec<&str> = found.iter().map(|c| c.account.as_str()).collect();
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
            let env = |_: &str| None;
            load(&LoadParams {
                launch_dir: &self.launch,
                config_dir: &self.config_dir,
                data_dir: &self.data,
                settings_file: None,
                cli_rules: CliRules {
                    allow: &[],
                    deny: &[],
                },
                env: &env,
            })
        }
    }

    const USER: &str = "# mine\n[accounts.local]\nkind = \"ollama\"\n\n[tui]\ntheme = \"dark\"\n";

    fn offer(posture: Posture) -> Offer {
        Offer {
            account: "local".to_owned(),
            benchmark: &BENCHMARKS[0],
            posture,
        }
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
                check_preset_params("screen", &params)
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
            assert_eq!(composition.params[set].as_f64(), Some(offer.threshold()));
            assert!(!composition.params.contains_key(unset));
        }
    }

    #[test]
    fn a_section_that_would_not_load_is_refused_and_nothing_is_written() {
        let world =
            World::new("[accounts.cloud]\nkind = \"openai-compatible\"\nbase_url = \"http://x\"\n");
        let config = world.load().unwrap();
        let before = std::fs::read_to_string(world.user_file()).unwrap();
        let mut wrong = offer(Posture::Safe);
        wrong.account = "cloud".to_owned();
        let err = write(&world.user_file(), &config, &wrong).unwrap_err();
        assert!(err.to_string().contains("ollama"), "{err}");
        let mut missing = offer(Posture::Safe);
        missing.account = "absent".to_owned();
        write(&world.user_file(), &config, &missing).unwrap_err();
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
}
