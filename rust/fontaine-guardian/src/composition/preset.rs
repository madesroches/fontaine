//! The built-in compositions. A preset is a Rune script plus the reads it declares and the
//! parameters it takes; the thresholds are fitted to the reads' exact wording, so the reads are
//! fixed here rather than configured.

use std::collections::{BTreeMap, HashMap};

use super::{CompositionError, Param};
use crate::screen::{DEFAULT_STEERING_VETO, Prompt, Question, Read};

pub const SCREEN: &str = "screen";

/// The names `preset` accepts.
pub const NAMES: &[&str] = &[SCREEN];

pub(crate) struct Resolved {
    pub source: &'static str,
    pub reads: BTreeMap<String, Read>,
    pub params: HashMap<String, Param>,
}

/// The reads a preset declares on `model`; `question` is the screen preset's stop question.
pub(crate) fn declared_reads(
    name: &str,
    model: &str,
    params: &BTreeMap<String, Param>,
) -> Result<BTreeMap<String, Read>, CompositionError> {
    match name {
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

pub(crate) fn resolve(
    name: &str,
    model: &str,
    params: &BTreeMap<String, Param>,
    shadow: bool,
) -> Result<Resolved, CompositionError> {
    let reads = declared_reads(name, model, params)?;
    match name {
        SCREEN => {
            const KNOWN: &[&str] = &[
                "question",
                "stop_threshold",
                "allow_threshold",
                "steering_veto",
                "concurrent",
            ];
            if let Some(key) = params.keys().find(|k| !KNOWN.contains(&k.as_str())) {
                return Err(CompositionError::at(
                    key,
                    format!(
                        "unknown parameter of the screen preset; expected one of: {}",
                        KNOWN.join(", ")
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
            let concurrent = match params.get("concurrent") {
                None | Some(Param::Unset) => false,
                Some(Param::Bool(b)) => *b,
                Some(_) => return Err(CompositionError::at("concurrent", "must be a boolean")),
            };
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
