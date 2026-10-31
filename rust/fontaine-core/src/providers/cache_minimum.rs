//! The shortest prompt prefix Claude models cache. A cache marker on a shorter prefix does
//! nothing, and the provider says so only by reporting zero cache tokens.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Haiku,
    Opus,
    Sonnet,
}

/// Minimum tokens by (family, major, minor); a new model is one line.
const BY_VERSION: &[(Family, u32, u32, u32)] = &[
    (Family::Haiku, 3, 5, 2048),
    (Family::Haiku, 4, 5, 4096),
    (Family::Opus, 4, 5, 4096),
    (Family::Opus, 4, 6, 4096),
    (Family::Opus, 4, 7, 2048),
];

const SONNET_4: u32 = 1024;
const CLAUDE_5: u32 = 512;

/// The minimum cacheable prefix, in tokens, of a Claude model on a kind that forwards cache
/// markers to Anthropic. `None` for any other kind or model, and for a Claude release the table
/// does not list.
pub fn min_cacheable_tokens(kind: &str, model: &str) -> Option<u32> {
    if !matches!(kind, "anthropic" | "openrouter") {
        return None;
    }
    // OpenRouter ids are dotted (`claude-haiku-4.5`); Anthropic's are dashed.
    let id = model
        .strip_prefix("anthropic/")
        .unwrap_or(model)
        .replace('.', "-");
    let rest = id.strip_prefix("claude-")?;
    let mut family = None;
    let mut numbers = Vec::new();
    for part in rest.split('-') {
        match part {
            "haiku" => family = Some(Family::Haiku),
            "opus" => family = Some(Family::Opus),
            "sonnet" => family = Some(Family::Sonnet),
            // Longer digit runs are release dates.
            _ => {
                if let Ok(n) = part.parse::<u32>()
                    && part.len() <= 2
                {
                    numbers.push(n);
                }
            }
        }
    }
    let major = *numbers.first()?;
    let minor = numbers.get(1).copied().unwrap_or(0);
    if major >= 5 {
        return Some(CLAUDE_5);
    }
    let family = family?;
    if family == Family::Sonnet && major == 4 {
        return Some(SONNET_4);
    }
    BY_VERSION
        .iter()
        .find(|(f, ma, mi, _)| *f == family && *ma == major && *mi == minor)
        .map(|(.., tokens)| *tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_case_per_tier() {
        assert_eq!(
            min_cacheable_tokens("anthropic", "claude-opus-5-1"),
            Some(512)
        );
        assert_eq!(
            min_cacheable_tokens("anthropic", "claude-sonnet-5"),
            Some(512)
        );
        assert_eq!(
            min_cacheable_tokens("anthropic", "claude-sonnet-4-5"),
            Some(1024)
        );
        assert_eq!(
            min_cacheable_tokens("anthropic", "claude-sonnet-4-20250514"),
            Some(1024)
        );
        assert_eq!(
            min_cacheable_tokens("anthropic", "claude-3-5-haiku-20241022"),
            Some(2048)
        );
        assert_eq!(
            min_cacheable_tokens("anthropic", "claude-haiku-3-5"),
            Some(2048)
        );
        assert_eq!(
            min_cacheable_tokens("anthropic", "claude-opus-4-7"),
            Some(2048)
        );
        assert_eq!(
            min_cacheable_tokens("anthropic", "claude-opus-4-5"),
            Some(4096)
        );
        assert_eq!(
            min_cacheable_tokens("anthropic", "claude-opus-4-6"),
            Some(4096)
        );
        assert_eq!(
            min_cacheable_tokens("anthropic", "claude-haiku-4-5-20251001"),
            Some(4096)
        );
    }

    #[test]
    fn openrouter_ids_are_dotted_and_prefixed() {
        assert_eq!(
            min_cacheable_tokens("openrouter", "anthropic/claude-haiku-4.5"),
            Some(4096)
        );
        assert_eq!(
            min_cacheable_tokens("openrouter", "anthropic/claude-3.5-haiku"),
            Some(2048)
        );
    }

    #[test]
    fn unknown_and_non_anthropic_ids_have_no_minimum() {
        assert_eq!(min_cacheable_tokens("openrouter", "openai/gpt-5.2"), None);
        assert_eq!(
            min_cacheable_tokens("openrouter", "anthropic/claude-mystery"),
            None
        );
        assert_eq!(min_cacheable_tokens("anthropic", "claude-opus-4"), None);
        assert_eq!(min_cacheable_tokens("openai", "claude-opus-4-5"), None);
        assert_eq!(min_cacheable_tokens("opencode-go", "claude-opus-4-5"), None);
    }
}
