//! Model facts for an id the catalog does not list, taken from the newest listed model of the
//! same id shape: the id with its version numbers taken out. Only an id whose shape the vendor
//! already lists gets facts; any other id is never guessed at.

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

const SEPARATORS: [char; 4] = ['-', '_', '/', ':'];

/// `id` with each run of version tokens (digits and dots only, such as `5`, `5.4` or
/// `20250929`) replaced by one `#`: `claude-sonnet-4-5-20250929` and `claude-sonnet-5` share
/// `claude-sonnet-#`. `None` for an id with no version token, which says nothing about which
/// listed model it is a release of.
fn shape(id: &str) -> Option<String> {
    let is_version = |token: &str| {
        token.chars().any(|c| c.is_ascii_digit())
            && token.chars().all(|c| c.is_ascii_digit() || c == '.')
    };
    let separators: Vec<char> = id.chars().filter(|c| SEPARATORS.contains(c)).collect();
    let mut shape = String::with_capacity(id.len());
    let mut versioned = false;
    let mut after_version = false;
    for (i, token) in id.split(SEPARATORS).enumerate() {
        let version = is_version(token);
        if version && after_version {
            continue;
        }
        if let Some(separator) = i.checked_sub(1).and_then(|i| separators.get(i)) {
            shape.push(*separator);
        }
        shape.push_str(if version { "#" } else { token });
        after_version = version;
        versioned |= version;
    }
    versioned.then_some(shape)
}

/// The facts of `model` on `vendor`, or `None` when no usable listed model shares its shape.
pub fn derive(
    catalog: &Catalog,
    vendor: ProviderId,
    model: &str,
    overlay: &Capabilities,
) -> Option<Derived> {
    let wanted = shape(model)?;
    // `None` ranks below any date, so an undated entry is the base only when nothing is dated.
    let base = catalog
        .iter()
        .filter(|spec| {
            spec.provider.vendor() == vendor.vendor()
                && !spec.deprecated
                && spec.max_output_tokens.is_some()
                && shape(&spec.id).as_deref() == Some(wanted.as_str())
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
    spec.details.modes.clear();
    overlay_onto(&mut spec, overlay);
    Some(Derived {
        facts: ModelFacts::new(spec).with_catalog(catalog),
        from: base.id.clone(),
    })
}

/// The derived facts of `model` on `vendor` when the built-in catalog does not list it; `None`
/// for a listed model, which keeps its own, and for one with no listed model of its shape.
pub fn unlisted(vendor: ProviderId, model: &str) -> Option<Derived> {
    let catalog = Catalog::builtin();
    if catalog.get(vendor, model).is_some() {
        return None;
    }
    derive(catalog, vendor, model, &Capabilities::default())
}

/// [`unlisted`]'s facts alone, for a wire to bind.
pub fn unlisted_facts(vendor: ProviderId, model: &str) -> Option<ModelFacts> {
    unlisted(vendor, model).map(|derived| derived.facts)
}

/// The one-time warning for a model whose facts were derived: `model` as selected, `resolved`
/// the id the catalog was asked for, `from` the listed id the facts came from, and `also` any
/// other source laid over them.
pub fn notice(model: &str, resolved: &str, from: &str, also: Option<&str>) -> String {
    let subject = if model == resolved {
        format!("Model `{model}` is not listed by this build of Fontaine")
    } else {
        format!(
            "Model `{model}` resolves to `{resolved}`, which this build of Fontaine does not list"
        )
    };
    let also = also.map(|also| format!(" and {also}")).unwrap_or_default();
    format!(
        "{subject}; its limits and features are taken from `{from}`{also}. Update Fontaine for \
         its exact facts."
    )
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

    fn entry(id: &str, date: Option<&str>) -> ModelSpec {
        let mut spec = ModelSpec::new(anthropic(), id).with_max_output_tokens(1000);
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
    fn a_shape_takes_out_each_run_of_version_tokens() {
        let shape_of = |id| shape(id);
        assert_eq!(
            shape_of("claude-haiku-5-5").as_deref(),
            Some("claude-haiku-#")
        );
        assert_eq!(
            shape_of("claude-sonnet-4-5-20250929").as_deref(),
            Some("claude-sonnet-#")
        );
        assert_eq!(shape_of("gpt-5.4-mini").as_deref(), Some("gpt-#-mini"));
        assert_eq!(shape_of("gpt-5-2025-08-07").as_deref(), Some("gpt-#"));
        assert_eq!(
            shape_of("anthropic/claude-sonnet-4.5").as_deref(),
            Some("anthropic/claude-sonnet-#")
        );
        assert_eq!(shape_of("qwen_2:7").as_deref(), Some("qwen_#"));
        assert_eq!(
            shape_of("gemini-2.5-flash-preview-09-2025").as_deref(),
            Some("gemini-#-flash-preview-#")
        );
    }

    #[test]
    fn an_id_without_a_version_token_has_no_shape() {
        for id in ["gpt-4o", "o3-mini", "qwen3:4b", "claude-sonnet", "...", ""] {
            assert_eq!(shape(id), None, "{id}");
        }
        assert!(
            derived(
                Catalog::builtin(),
                "claude-sonnet",
                &Capabilities::default()
            )
            .is_none()
        );
    }

    #[test]
    fn newest_listed_model_of_the_shape_is_the_base() {
        let catalog = catalog(vec![
            entry("zz-1", Some("2025-01-01")),
            entry("zz-3", Some("2026-03")),
            entry("zz-2", Some("2025-09-01")),
            entry("zz-4-fast", Some("2027-01-01")),
        ]);
        let d = derived(&catalog, "zz-9-9", &Capabilities::default()).expect("derived");
        assert_eq!(d.from, "zz-3");
        assert_eq!(spec_of(&d).id, "zz-9-9");
        assert_eq!(spec_of(&d).display_name, "zz-9-9");
        assert_eq!(spec_of(&d).max_output_tokens, Some(1000));
        let d = derived(&catalog, "zz-9-fast", &Capabilities::default()).expect("derived");
        assert_eq!(d.from, "zz-4-fast");
    }

    #[test]
    fn builtin_haiku_derives_from_the_newest_listed_haiku() {
        let d = derived(
            Catalog::builtin(),
            "claude-haiku-9-9",
            &Capabilities::default(),
        )
        .expect("derived");
        let newest = Catalog::builtin()
            .iter()
            .filter(|spec| {
                spec.provider.vendor() == "anthropic"
                    && spec.id.starts_with("claude-haiku-")
                    && !spec.deprecated
                    && spec.max_output_tokens.is_some()
            })
            .max_by(|a, b| (&a.details.release_date, &a.id).cmp(&(&b.details.release_date, &b.id)))
            .expect("rig lists a haiku");
        assert_eq!(d.from, newest.id);
        assert!(spec_of(&d).max_output_tokens.is_some());
    }

    #[test]
    fn builtin_openai_mini_derives_from_a_listed_mini() {
        let openai = ProviderId::catalog("openai").expect("rig lists openai");
        let d = derive(
            Catalog::builtin(),
            openai,
            "gpt-9.9-mini",
            &Capabilities::default(),
        )
        .expect("derived");
        assert_eq!(shape(&d.from).as_deref(), Some("gpt-#-mini"), "{}", d.from);
        assert!(unlisted(openai, "gpt-9.9-mini").is_some());
    }

    #[test]
    fn a_listed_model_is_not_derived_by_the_catalog_miss_helper() {
        assert!(unlisted(anthropic(), "claude-haiku-4-5").is_none());
        assert!(unlisted(anthropic(), "claude-haiku-4-5-20251001").is_none());
        assert!(unlisted_facts(anthropic(), "claude-haiku-9-9").is_some());
    }

    #[test]
    fn another_vendors_listing_is_never_the_base() {
        let openai = ProviderId::catalog("openai").expect("rig lists openai");
        assert!(
            derive(
                Catalog::builtin(),
                openai,
                "claude-haiku-9-9",
                &Capabilities::default()
            )
            .is_none()
        );
    }

    #[test]
    fn deprecated_limitless_and_undated_entries_lose() {
        let mut deprecated = entry("zz-8", Some("2027-01-01"));
        deprecated.deprecated = true;
        let mut limitless = entry("zz-7", Some("2027-02-01"));
        limitless.max_output_tokens = None;
        let catalog = catalog(vec![
            deprecated,
            limitless,
            entry("zz-6", None),
            entry("zz-1", Some("2020-01-01")),
        ]);
        let d = derived(&catalog, "zz-9", &Capabilities::default()).expect("derived");
        assert_eq!(d.from, "zz-1");
    }

    #[test]
    fn undated_is_the_base_only_when_alone_and_ties_take_the_greater_id() {
        let alone = catalog(vec![entry("yy-1", None)]);
        let d = derived(&alone, "yy-9", &Capabilities::default()).expect("derived");
        assert_eq!(d.from, "yy-1");
        let tied = catalog(vec![
            entry("xx-1", Some("2026-01-01")),
            entry("xx-2", Some("2026-01-01")),
        ]);
        let d = derived(&tied, "xx-9", &Capabilities::default()).expect("derived");
        assert_eq!(d.from, "xx-2");
    }

    #[test]
    fn an_unknown_shape_and_a_shape_without_a_usable_entry_are_none() {
        assert!(
            derived(
                Catalog::builtin(),
                "claude-quantum-9",
                &Capabilities::default()
            )
            .is_none()
        );
        let mut dep = entry("ww-1", Some("2026-01-01"));
        dep.deprecated = true;
        let catalog = catalog(vec![dep]);
        assert!(derived(&catalog, "ww-9", &Capabilities::default()).is_none());
    }

    #[test]
    fn the_notice_names_the_base_and_any_overlay() {
        assert_eq!(
            notice("m-9", "m-9", "m-1", None),
            "Model `m-9` is not listed by this build of Fontaine; its limits and features are \
             taken from `m-1`. Update Fontaine for its exact facts."
        );
        assert_eq!(
            notice("m", "m-9", "m-1", Some("the flags")),
            "Model `m` resolves to `m-9`, which this build of Fontaine does not list; its limits \
             and features are taken from `m-1` and the flags. Update Fontaine for its exact facts."
        );
    }

    #[test]
    fn price_and_dates_are_cleared() {
        let mut base = entry("vv-1", Some("2026-01-01"));
        base.pricing = Some(rig_core::catalog::Pricing::new(1.0, 2.0));
        base.details.last_updated = Some("2026-02-01".into());
        base.details.canonical_model_id = Some("vv-1".into());
        base.details.family = Some("vv".into());
        base.details.modes.insert("fast".into(), Default::default());
        base.details.cost.reasoning = Some(3.0);
        let catalog = catalog(vec![base]);
        let d = derived(&catalog, "vv-9", &Capabilities::default()).expect("derived");
        let spec = spec_of(&d);
        assert!(spec.pricing.is_none());
        assert_eq!(spec.details.cost, Default::default());
        assert!(spec.details.release_date.is_none());
        assert!(spec.details.last_updated.is_none());
        assert!(spec.details.canonical_model_id.is_none());
        assert!(spec.details.modes.is_empty());
        assert_eq!(spec.details.family.as_deref(), Some("vv"));
    }

    fn listed_base(default: Option<Effort>, can_disable: bool) -> Catalog {
        let mut base = entry("uu-1", Some("2026-01-01"));
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
            .get_exact(anthropic(), "uu-1")
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
        let catalog = catalog(vec![entry("tt-1", Some("2026-01-01"))]);
        let overlay = Capabilities {
            thinking_off: Some(true),
            ..Capabilities::default()
        };
        let d = derived(&catalog, "tt-9", &overlay).expect("derived");
        assert_eq!(spec_of(&d).reasoning, ReasoningSupport::None);
    }

    #[test]
    fn levels_make_a_non_reasoning_base_listed() {
        let catalog = catalog(vec![entry("ss-1", Some("2026-01-01"))]);
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
