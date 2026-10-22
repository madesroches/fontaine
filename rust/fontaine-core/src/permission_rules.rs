use std::fmt;

use fontaine_tools::names::{is_builtin, is_mcp};

/// A parsed `Tool` / `Tool(specifier)` rule. Specifier parsing and matching are not
/// implemented here: apart from the shapes validated at parse time, a specifier is kept raw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub tool: String,
    pub specifier: Option<String>,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.specifier {
            Some(spec) => write!(f, "{}({spec})", self.tool),
            None => f.write_str(&self.tool),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleError {
    pub rule: String,
    pub reason: String,
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid permission rule `{}`: {}",
            self.rule, self.reason
        )
    }
}

impl std::error::Error for RuleError {}

pub fn parse_rule(text: &str) -> Result<Rule, RuleError> {
    let fail = |reason: &str| RuleError {
        rule: text.to_string(),
        reason: reason.to_string(),
    };

    let (tool, specifier) = match text.find('(') {
        None => (text, None),
        Some(open) => {
            if !text.ends_with(')') {
                return Err(fail("expected `Tool(specifier)` with a closing `)`"));
            }
            let inner = &text[open + 1..text.len() - 1];
            if inner.trim().is_empty() {
                return Err(fail("empty specifier; write the bare tool name instead"));
            }
            (&text[..open], Some(inner))
        }
    };

    if tool.is_empty() {
        return Err(fail("missing tool name"));
    }
    if !tool
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || (c == '*' && is_mcp(tool)))
    {
        return Err(fail(
            "tool names contain only letters, digits, `_` and `-` (`*` only ends an `mcp__` name)",
        ));
    }

    if is_mcp(tool) {
        validate_mcp_name(tool).map_err(|reason| fail(&reason))?;
        if specifier.is_some() {
            return Err(fail("MCP rules take no specifier"));
        }
    } else if !is_builtin(tool) {
        return Err(fail(
            "unknown tool; expected a built-in tool name or an `mcp__<server>[__<tool>]` name",
        ));
    }

    if tool == "webfetch"
        && let Some(spec) = specifier
    {
        validate_domain_specifier(spec).map_err(|reason| fail(&reason))?;
    }

    Ok(Rule {
        tool: tool.to_string(),
        specifier: specifier.map(str::to_string),
    })
}

pub(crate) fn validate_mcp_name(name: &str) -> Result<(), String> {
    let rest = &name[fontaine_tools::names::MCP_TOOL_PREFIX.len()..];
    if !rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '*')
    {
        return Err(
            "MCP names contain only letters, digits, `_`, `-` and a trailing `*`".to_string(),
        );
    }
    let (server, tool) = match rest.split_once("__") {
        Some((server, tool)) => (server, Some(tool)),
        None => (rest, None),
    };
    if server.is_empty() || server.contains('*') {
        return Err("expected `mcp__<server>` with a non-empty server name".to_string());
    }
    if let Some(tool) = tool
        && (tool.is_empty() || (tool.contains('*') && tool != "*"))
    {
        return Err("expected `mcp__<server>__<tool>` or `mcp__<server>__*`".to_string());
    }
    Ok(())
}

fn validate_domain_specifier(spec: &str) -> Result<(), String> {
    let form = "webfetch rules take `domain:<host>` or `domain:*.<host>`".to_string();
    let Some(host) = spec.strip_prefix("domain:") else {
        return Err(form);
    };
    let host = host.strip_prefix("*.").unwrap_or(host);
    if host.is_empty()
        || host.contains(|c: char| c.is_whitespace() || matches!(c, '/' | ':' | '*' | '@'))
    {
        return Err(form);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Rule {
        parse_rule(text).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn bare_and_specifier_forms() {
        assert_eq!(
            ok("shell"),
            Rule {
                tool: "shell".into(),
                specifier: None
            }
        );
        assert_eq!(
            ok("shell(git push *)").specifier.as_deref(),
            Some("git push *")
        );
        assert_eq!(
            ok("read(~/.aws/**)").specifier.as_deref(),
            Some("~/.aws/**")
        );
        assert_eq!(
            ok("shell(echo (nested))").specifier.as_deref(),
            Some("echo (nested)")
        );
        assert_eq!(ok("shell(rm)").to_string(), "shell(rm)");
    }

    #[test]
    fn mcp_names() {
        for rule in ["mcp__github", "mcp__github__create_issue", "mcp__github__*"] {
            ok(rule);
        }
        for rule in [
            "mcp__",
            "mcp____tool",
            "mcp__github__",
            "mcp__git*__x",
            "mcp__github__cre*",
            "mcp__github(x)",
        ] {
            assert!(parse_rule(rule).is_err(), "{rule}");
        }
    }

    #[test]
    fn webfetch_domain_form() {
        ok("webfetch");
        ok("webfetch(domain:example.com)");
        ok("webfetch(domain:*.example.com)");
        for rule in [
            "webfetch(example.com)",
            "webfetch(domain:)",
            "webfetch(domain:*.)",
            "webfetch(domain:http://example.com)",
            "webfetch(domain:a.com/path)",
        ] {
            assert!(parse_rule(rule).is_err(), "{rule}");
        }
    }

    #[test]
    fn rejects_bad_shapes_and_unknown_tools() {
        for rule in [
            "", "(x)", "shell(", "shell()", "shell( )", "shell(x", "Shell", "bash", "shell x",
            "shell)(",
        ] {
            assert!(parse_rule(rule).is_err(), "{rule:?}");
        }
    }
}
