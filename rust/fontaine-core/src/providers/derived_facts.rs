//! Model facts for an id the catalog does not list, taken from the closest listed model of the
//! same family. Only a model whose family is already listed gets facts: an id with no known family
//! is never guessed at.

use rig_core::catalog::{Catalog, ModelFacts, ModelSpec, ReasoningSupport};
use rig_core::completion::Effort;
use rig_core::providers::registry::ProviderId;

/// What a provider reports about a model's reasoning controls, laid over the base's facts. A
/// field the provider did not report, or reported with the wrong type, is `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// The effort levels the model takes, by name.
    pub effort_levels: Option<Vec<String>>,
    /// Whether an effort level goes with adaptive thinking.
    pub adaptive_thinking: Option<bool>,
    /// Whether a request can turn the model's reasoning off.
    pub thinking_off: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct Derived {
    pub facts: ModelFacts,
    /// The catalog id the facts were taken from.
    pub from: String,
}

fn effort(name: &str) -> Option<Effort> {
    Some(match name {
        "minimal" => Effort::Minimal,
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "high" => Effort::High,
        "xhigh" => Effort::XHigh,
        "max" => Effort::Max,
        _ => return None,
    })
}

/// The facts of `model` on `vendor`, or `None` when no listed family prefixes it or the family
/// has no usable entry to copy.
pub fn derive(
    catalog: &Catalog,
    vendor: ProviderId,
    model: &str,
    overlay: &Capabilities,
) -> Option<Derived> {
    let of_vendor = || {
        catalog
            .iter()
            .filter(|spec| spec.provider.vendor() == vendor.vendor())
    };
    let family = of_vendor()
        .filter_map(|spec| spec.details.family.as_deref())
        .filter(|family| {
            model
                .strip_prefix(family)
                .is_some_and(|rest| rest.starts_with('-'))
        })
        .max_by_key(|family| family.len())?;
    // `None` ranks below any date, so an undated entry is the base only when nothing is dated.
    let base = of_vendor()
        .filter(|spec| {
            spec.details.family.as_deref() == Some(family)
                && !spec.deprecated
                && spec.max_output_tokens.is_some()
        })
        .max_by(|a, b| (&a.details.release_date, &a.id).cmp(&(&b.details.release_date, &b.id)))?;

    let mut spec: ModelSpec = base.clone();
    spec.id = model.to_string();
    spec.display_name = model.to_string();
    spec.pricing = None;
    spec.details.cost = Default::default();
    spec.details.release_date = None;
    spec.details.last_updated = None;
    spec.details.canonical_model_id = None;
    overlay_onto(&mut spec, overlay);
    Some(Derived {
        facts: ModelFacts::new(spec).with_catalog(catalog),
        from: base.id.clone(),
    })
}

fn overlay_onto(spec: &mut ModelSpec, overlay: &Capabilities) {
    // A level it cannot name is dropped, never claimed.
    let levels: Option<Vec<Effort>> = overlay
        .effort_levels
        .as_ref()
        .map(|names| names.iter().filter_map(|name| effort(name)).collect())
        .filter(|levels: &Vec<Effort>| !levels.is_empty());
    let base_listed = match &spec.reasoning {
        ReasoningSupport::Listed {
            can_disable,
            default,
            budget,
            levels,
        } => Some((levels.clone(), budget.clone(), *can_disable, *default)),
        _ => None,
    };
    if levels.is_some() || base_listed.is_some() {
        let base_can_disable = base_listed.as_ref().is_some_and(|b| b.2);
        let can_disable = overlay.thinking_off.unwrap_or(base_can_disable);
        spec.reasoning = match (levels, base_listed) {
            (Some(levels), base) => {
                let default = base
                    .and_then(|b| b.3)
                    .filter(|default| levels.contains(default));
                ReasoningSupport::Listed {
                    levels,
                    budget: None,
                    can_disable,
                    default,
                }
            }
            (None, Some((levels, budget, _, default))) => ReasoningSupport::Listed {
                levels,
                budget,
                can_disable,
                default,
            },
            (None, None) => unreachable!("one of the two is present"),
        };
    }
    if let Some(adaptive) = overlay.adaptive_thinking {
        spec.compat.adaptive_thinking = adaptive;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anthropic() -> ProviderId {
        ProviderId::catalog("anthropic").expect("rig lists anthropic")
    }

    fn entry(id: &str, family: Option<&str>, date: Option<&str>) -> ModelSpec {
        let mut spec = ModelSpec::new(anthropic(), id).with_max_output_tokens(1000);
        spec.details.family = family.map(str::to_string);
        spec.details.release_date = date.map(str::to_string);
        spec
    }

    fn catalog(specs: Vec<ModelSpec>) -> Catalog {
        let mut catalog = Catalog::builtin().clone();
        for spec in specs {
            catalog.insert(spec);
        }
        catalog
    }

    fn derived(catalog: &Catalog, model: &str, overlay: &Capabilities) -> Option<Derived> {
        derive(catalog, anthropic(), model, overlay)
    }

    fn spec_of(d: &Derived) -> &ModelSpec {
        d.facts.spec().expect("bound spec")
    }

    #[test]
    fn newest_listed_sibling_is_the_base() {
        let catalog = catalog(vec![
            entry("zz-old", Some("zz"), Some("2025-01-01")),
            entry("zz-new", Some("zz"), Some("2026-03")),
            entry("zz-mid", Some("zz"), Some("2025-09-01")),
        ]);
        let d = derived(&catalog, "zz-9-9", &Capabilities::default()).expect("derived");
        assert_eq!(d.from, "zz-new");
        assert_eq!(spec_of(&d).id, "zz-9-9");
        assert_eq!(spec_of(&d).display_name, "zz-9-9");
        assert_eq!(spec_of(&d).max_output_tokens, Some(1000));
    }

    #[test]
    fn builtin_haiku_derives_from_a_listed_haiku() {
        let d = derived(
            Catalog::builtin(),
            "claude-haiku-9-9",
            &Capabilities::default(),
        )
        .expect("derived");
        assert!(d.from.starts_with("claude-haiku-"), "{}", d.from);
        assert!(spec_of(&d).max_output_tokens.is_some());
    }

    #[test]
    fn longest_family_prefix_wins() {
        let catalog = catalog(vec![
            entry("zz-a", Some("zz"), Some("2026-01-01")),
            entry("zz-fast-a", Some("zz-fast"), Some("2024-01-01")),
        ]);
        let d = derived(&catalog, "zz-fast-9", &Capabilities::default()).expect("derived");
        assert_eq!(d.from, "zz-fast-a");
    }

    #[test]
    fn family_must_end_at_a_dash() {
        let catalog = catalog(vec![entry("zz-a", Some("zz"), Some("2026-01-01"))]);
        assert!(derived(&catalog, "zzz-9", &Capabilities::default()).is_none());
        assert!(derived(&catalog, "zz", &Capabilities::default()).is_none());
    }

    #[test]
    fn deprecated_limitless_and_undated_entries_lose() {
        let mut deprecated = entry("zz-dep", Some("zz"), Some("2027-01-01"));
        deprecated.deprecated = true;
        let mut limitless = entry("zz-nolimit", Some("zz"), Some("2027-02-01"));
        limitless.max_output_tokens = None;
        let catalog = catalog(vec![
            deprecated,
            limitless,
            entry("zz-undated", Some("zz"), None),
            entry("zz-dated", Some("zz"), Some("2020-01-01")),
        ]);
        let d = derived(&catalog, "zz-9", &Capabilities::default()).expect("derived");
        assert_eq!(d.from, "zz-dated");
    }

    #[test]
    fn undated_is_the_base_only_when_alone_and_ties_take_the_greater_id() {
        let alone = catalog(vec![entry("yy-a", Some("yy"), None)]);
        let d = derived(&alone, "yy-9", &Capabilities::default()).expect("derived");
        assert_eq!(d.from, "yy-a");
        let tied = catalog(vec![
            entry("xx-a", Some("xx"), Some("2026-01-01")),
            entry("xx-b", Some("xx"), Some("2026-01-01")),
        ]);
        let d = derived(&tied, "xx-9", &Capabilities::default()).expect("derived");
        assert_eq!(d.from, "xx-b");
    }

    #[test]
    fn unknown_family_and_family_without_a_usable_entry_are_none() {
        assert!(
            derived(
                Catalog::builtin(),
                "claude-quantum-9",
                &Capabilities::default()
            )
            .is_none()
        );
        let mut dep = entry("ww-a", Some("ww"), Some("2026-01-01"));
        dep.deprecated = true;
        let catalog = catalog(vec![dep]);
        assert!(derived(&catalog, "ww-9", &Capabilities::default()).is_none());
    }

    #[test]
    fn price_and_dates_are_cleared() {
        let mut base = entry("vv-a", Some("vv"), Some("2026-01-01"));
        base.pricing = Some(rig_core::catalog::Pricing::new(1.0, 2.0));
        base.details.last_updated = Some("2026-02-01".into());
        base.details.canonical_model_id = Some("vv-a".into());
        base.details.cost.reasoning = Some(3.0);
        let catalog = catalog(vec![base]);
        let d = derived(&catalog, "vv-9", &Capabilities::default()).expect("derived");
        let spec = spec_of(&d);
        assert!(spec.pricing.is_none());
        assert_eq!(spec.details.cost, Default::default());
        assert!(spec.details.release_date.is_none());
        assert!(spec.details.last_updated.is_none());
        assert!(spec.details.canonical_model_id.is_none());
        assert_eq!(spec.details.family.as_deref(), Some("vv"));
    }

    fn listed_base(default: Option<Effort>, can_disable: bool) -> Catalog {
        let mut base = entry("uu-a", Some("uu"), Some("2026-01-01"));
        base.reasoning = ReasoningSupport::Listed {
            levels: vec![Effort::Low, Effort::High],
            budget: Some(1..=10),
            can_disable,
            default,
        };
        base.compat.thinking_off = Some("off".into());
        catalog(vec![base])
    }

    #[test]
    fn overlay_maps_known_levels_and_drops_unknown_ones() {
        let catalog = listed_base(Some(Effort::High), false);
        let overlay = Capabilities {
            effort_levels: Some(["low", "turbo", "xhigh", "max"].map(String::from).to_vec()),
            adaptive_thinking: Some(true),
            thinking_off: Some(true),
        };
        let d = derived(&catalog, "uu-9", &overlay).expect("derived");
        let spec = spec_of(&d);
        assert_eq!(
            spec.reasoning,
            ReasoningSupport::Listed {
                levels: vec![Effort::Low, Effort::XHigh, Effort::Max],
                budget: None,
                can_disable: true,
                default: None,
            }
        );
        assert!(spec.compat.adaptive_thinking);
        assert_eq!(spec.compat.thinking_off.as_deref(), Some("off"));
    }

    #[test]
    fn base_default_is_kept_only_when_listed() {
        let catalog = listed_base(Some(Effort::High), true);
        let overlay = Capabilities {
            effort_levels: Some(vec!["low".into(), "high".into(), "max".into()]),
            ..Capabilities::default()
        };
        let d = derived(&catalog, "uu-9", &overlay).expect("derived");
        assert_eq!(
            spec_of(&d).reasoning,
            ReasoningSupport::Listed {
                levels: vec![Effort::Low, Effort::High, Effort::Max],
                budget: None,
                can_disable: true,
                default: Some(Effort::High),
            }
        );
    }

    #[test]
    fn absent_overlay_keeps_the_base_reasoning() {
        let catalog = listed_base(Some(Effort::High), false);
        let d = derived(&catalog, "uu-9", &Capabilities::default()).expect("derived");
        assert_eq!(spec_of(&d).reasoning, catalog_base(&catalog).reasoning);
    }

    fn catalog_base(catalog: &Catalog) -> &ModelSpec {
        catalog
            .get_exact(anthropic(), "uu-a")
            .expect("the base is listed")
    }

    #[test]
    fn levels_with_no_known_name_count_as_absent() {
        let catalog = listed_base(Some(Effort::High), false);
        let overlay = Capabilities {
            effort_levels: Some(vec!["turbo".into()]),
            thinking_off: Some(true),
            ..Capabilities::default()
        };
        let d = derived(&catalog, "uu-9", &overlay).expect("derived");
        assert_eq!(
            spec_of(&d).reasoning,
            ReasoningSupport::Listed {
                levels: vec![Effort::Low, Effort::High],
                budget: Some(1..=10),
                can_disable: true,
                default: Some(Effort::High),
            }
        );
    }

    #[test]
    fn a_base_that_does_not_reason_ignores_thinking_off() {
        let catalog = catalog(vec![entry("tt-a", Some("tt"), Some("2026-01-01"))]);
        let overlay = Capabilities {
            thinking_off: Some(true),
            ..Capabilities::default()
        };
        let d = derived(&catalog, "tt-9", &overlay).expect("derived");
        assert_eq!(spec_of(&d).reasoning, ReasoningSupport::None);
    }

    #[test]
    fn levels_make_a_non_reasoning_base_listed() {
        let catalog = catalog(vec![entry("ss-a", Some("ss"), Some("2026-01-01"))]);
        let overlay = Capabilities {
            effort_levels: Some(vec!["medium".into()]),
            ..Capabilities::default()
        };
        let d = derived(&catalog, "ss-9", &overlay).expect("derived");
        assert_eq!(
            spec_of(&d).reasoning,
            ReasoningSupport::Listed {
                levels: vec![Effort::Medium],
                budget: None,
                can_disable: false,
                default: None,
            }
        );
    }
}
