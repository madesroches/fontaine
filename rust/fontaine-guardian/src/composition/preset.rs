//! The built-in compositions. A preset is a Rune script plus the reads it declares and the
//! parameters it takes; the thresholds are fitted to the reads' exact wording, so the reads are
//! fixed here rather than configured.

use std::collections::{BTreeMap, HashMap};

use super::{CompositionError, Param};
use crate::screen::{BackendKind, DEFAULT_STEERING_VETO, Prompt, Question, Read};

pub const SCREEN: &str = "screen";

/// The names `preset` accepts.
pub const NAMES: &[&str] = &[SCREEN];

/// The stop threshold of the screen preset on a hosted model: the 5% stop point of the synthetic
/// corpus.
pub const JEV_STOP_THRESHOLD: f64 = 0.34;
/// The allow threshold of the screen preset on a hosted model: the synthetic corpus's zero-miss
/// skip point (0.30) less a 0.10 margin for the model's nondeterminism.
pub const JEV_ALLOW_THRESHOLD: f64 = 0.20;

/// Whether the Jev threshold defaults fit `model`, a `<account>/<vendor>/<model>` label. They are
/// fitted to Typesafe's models only; an unknown (empty) model is not rejected, so parameter
/// validation without a model keeps working.
pub fn has_default_thresholds(model: &str) -> bool {
    model.is_empty()
        || model
            .split_once('/')
            .is_some_and(|(_, id)| id.starts_with("typesafe/"))
}

pub(crate) struct Resolved {
    pub source: &'static str,
    pub reads: BTreeMap<String, Read>,
    pub params: HashMap<String, Param>,
}

/// The reads a preset declares on `model`; `question` is the screen preset's stop question on a
/// local model. A hosted model reads one question, asked for the stop and the allow alike.
pub(crate) fn declared_reads(
    name: &str,
    model: &str,
    backend: Option<BackendKind>,
    params: &BTreeMap<String, Param>,
) -> Result<BTreeMap<String, Read>, CompositionError> {
    match name {
        SCREEN if backend == Some(BackendKind::SystemOne) => {
            let read = || Read {
                model: model.to_owned(),
                prompt: Prompt::Plain,
                question: Question::Level5Employee,
                backend: BackendKind::SystemOne,
            };
            Ok(BTreeMap::from([
                ("stop".to_owned(), read()),
                ("allow".to_owned(), read()),
            ]))
        }
        SCREEN => {
            let question = match params.get("question") {
                None | Some(Param::Unset) => Question::EmployeeAllowed,
                Some(Param::Str(text)) => match text.as_str() {
                    "employee_allowed" => Question::EmployeeAllowed,
                    "outside_request" => Question::OutsideRequest,
                    other => {
                        return Err(CompositionError::at(
                            "question",
                            format!(
                                "unknown question `{other}`; expected employee_allowed or outside_request"
                            ),
                        ));
                    }
                },
                Some(_) => {
                    return Err(CompositionError::at("question", "must be a string"));
                }
            };
            let read = |prompt, question| Read {
                model: model.to_owned(),
                prompt,
                question,
                backend: BackendKind::Ollama,
            };
            Ok(BTreeMap::from([
                ("stop".to_owned(), read(Prompt::Plain, question)),
                (
                    "allow".to_owned(),
                    read(Prompt::Hardened, Question::EmployeeAllowed),
                ),
                (
                    "steering".to_owned(),
                    read(Prompt::Plain, Question::Steering),
                ),
            ]))
        }
        other => Err(CompositionError::new(format!(
            "unknown preset `{other}`; expected one of: {}",
            NAMES.join(", ")
        ))),
    }
}

fn number(
    params: &BTreeMap<String, Param>,
    key: &str,
    valid: impl Fn(f64) -> bool,
    expected: &str,
) -> Result<Option<f64>, CompositionError> {
    match params.get(key) {
        None | Some(Param::Unset) | Some(Param::Optional(None)) => Ok(None),
        Some(Param::Number(n) | Param::Optional(Some(n))) if valid(*n) => Ok(Some(*n)),
        Some(Param::Number(_) | Param::Optional(Some(_))) => {
            Err(CompositionError::at(key, expected))
        }
        Some(_) => Err(CompositionError::at(key, "must be a number")),
    }
}

fn boolean(
    params: &BTreeMap<String, Param>,
    key: &str,
    default: bool,
) -> Result<bool, CompositionError> {
    match params.get(key) {
        None | Some(Param::Unset) => Ok(default),
        Some(Param::Bool(b)) => Ok(*b),
        Some(_) => Err(CompositionError::at(key, "must be a boolean")),
    }
}

/// `backend` is `None` when the caller does not know the model's account: the parameters of either
/// backend are then accepted, and the reads are those of a local model.
pub(crate) fn resolve(
    name: &str,
    model: &str,
    backend: Option<BackendKind>,
    params: &BTreeMap<String, Param>,
    shadow: bool,
) -> Result<Resolved, CompositionError> {
    let reads = declared_reads(name, model, backend, params)?;
    match name {
        SCREEN => {
            const OLLAMA: &[&str] = &[
                "question",
                "stop_threshold",
                "allow_threshold",
                "steering_veto",
                "concurrent",
            ];
            const SYSTEMONE: &[&str] =
                &["stop_threshold", "allow_threshold", "allow", "concurrent"];
            const EITHER: &[&str] = &[
                "question",
                "stop_threshold",
                "allow_threshold",
                "allow",
                "steering_veto",
                "concurrent",
            ];
            let known = match backend {
                Some(BackendKind::Ollama) => OLLAMA,
                Some(BackendKind::SystemOne) => SYSTEMONE,
                None => EITHER,
            };
            if let Some(key) = params.keys().find(|k| !known.contains(&k.as_str())) {
                return Err(CompositionError::at(
                    key,
                    format!(
                        "unknown parameter of the screen preset on this model; expected one of: {}",
                        known.join(", ")
                    ),
                ));
            }
            let stop = number(
                params,
                "stop_threshold",
                |t| t > 0.0 && t <= 1.0,
                "must be greater than 0 and at most 1",
            )?;
            let allow = number(
                params,
                "allow_threshold",
                |t| t > 0.0 && t < 1.0,
                "must be greater than 0 and less than 1",
            )?;
            let veto = number(
                params,
                "steering_veto",
                |v| v > 0.0 && v <= 1.0,
                "must be greater than 0 and at most 1",
            )?;
            let concurrent = boolean(params, "concurrent", false)?;
            let allowing = boolean(params, "allow", true)?;
            if backend == Some(BackendKind::SystemOne) {
                // `allow = false` wins over a threshold: it is a switch, not a contradiction.
                let fitted = has_default_thresholds(model);
                if !fitted && !shadow && stop.is_none() && allow.is_none() {
                    return Err(CompositionError::at(
                        "allow_threshold",
                        format!(
                            "there is no default for `{model}`, whose thresholds are measured per model; set `allow_threshold` and/or `stop_threshold`, or keep `shadow = true` to log scores first"
                        ),
                    ));
                }
                let default = |value: f64| fitted.then_some(value);
                let allow = if allowing {
                    allow.or(default(JEV_ALLOW_THRESHOLD))
                } else {
                    None
                };
                return Ok(Resolved {
                    source: include_str!("screen_jev.rn"),
                    reads,
                    params: HashMap::from([
                        (
                            "stop_threshold".to_owned(),
                            Param::Optional(stop.or(default(JEV_STOP_THRESHOLD))),
                        ),
                        ("allow_threshold".to_owned(), Param::Optional(allow)),
                        ("concurrent".to_owned(), Param::Bool(concurrent)),
                    ]),
                });
            }
            if !shadow && stop.is_none() && allow.is_none() {
                return Err(CompositionError::at(
                    "allow_threshold",
                    "there is no default; set `allow_threshold` and/or `stop_threshold`, or keep `shadow = true` to log scores first",
                ));
            }
            Ok(Resolved {
                source: include_str!("screen.rn"),
                reads,
                params: HashMap::from([
                    ("stop_threshold".to_owned(), Param::Optional(stop)),
                    ("allow_threshold".to_owned(), Param::Optional(allow)),
                    (
                        "steering_veto".to_owned(),
                        Param::Number(veto.unwrap_or(DEFAULT_STEERING_VETO)),
                    ),
                    ("concurrent".to_owned(), Param::Bool(concurrent)),
                ]),
            })
        }
        other => Err(CompositionError::new(format!("unknown preset `{other}`"))),
    }
}
