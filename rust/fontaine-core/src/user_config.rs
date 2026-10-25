//! Edits to the user's settings file that Fontaine itself makes. The file is the user's own
//! document, so an edit goes through a format-preserving TOML editor and touches nothing but
//! the one value it adds.

use std::io;
use std::path::Path;

use toml_edit::{Array, DocumentMut, Item, Table, Value};

use crate::config::{Account, ThemeName};
use crate::permission_rules::parse_rule;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

// toml_edit's `Display` quotes the offending source line, which could echo an `[env]` value.
fn parse_document(text: &str, path: &Path) -> io::Result<DocumentMut> {
    text.parse().map_err(|e: toml_edit::TomlError| {
        invalid(format!(
            "{} is not valid TOML: {}",
            path.display(),
            crate::config::bare_toml_message(text, e.span(), e.message())
        ))
    })
}

/// Appends `rule` to `[permissions] allow` in the settings file at `path`, creating the file,
/// the table and the array as needed. A rule already listed is left alone. The file keeps its
/// comments, ordering and layout.
pub fn append_allow_rule(path: &Path, rule: &str) -> io::Result<()> {
    parse_rule(rule).map_err(|e| invalid(e.to_string()))?;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut doc = parse_document(&text, path)?;

    if doc.get("permissions").is_none() {
        doc.insert("permissions", Item::Table(Table::new()));
    }
    let permissions = doc["permissions"]
        .as_table_like_mut()
        .ok_or_else(|| invalid("`permissions` in the settings file is not a table"))?;
    if permissions.get("allow").is_none() {
        permissions.insert("allow", Item::Value(Value::Array(Array::new())));
    }
    let allow = permissions
        .get_mut("allow")
        .and_then(Item::as_array_mut)
        .ok_or_else(|| invalid("`permissions.allow` in the settings file is not an array"))?;
    if allow.iter().any(|v| v.as_str() == Some(rule)) {
        return Ok(());
    }
    allow.push(rule);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomically(path, doc.to_string().as_bytes())
}

/// Sets `[model] model` in the settings file at `path` to `reference` (`<account>/<model>`).
pub fn set_model(path: &Path, reference: &str) -> io::Result<()> {
    set_string(path, "model", "model", reference)
}

/// Sets `[guardian] model` in the settings file at `path` to `reference` (`<account>/<model>`).
pub fn set_guardian_model(path: &Path, reference: &str) -> io::Result<()> {
    set_string(path, "guardian", "model", reference)
}

/// Sets `[tui] theme` in the settings file at `path`.
pub fn set_theme(path: &Path, name: ThemeName) -> io::Result<()> {
    set_string(path, "tui", "theme", name.as_str())
}

/// Sets `table.key` to a string, creating the file and the table as needed and touching nothing
/// else in the file.
fn set_string(path: &Path, table: &str, key: &str, value: &str) -> io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut doc = parse_document(&text, path)?;
    if doc.get(table).is_none() {
        doc.insert(table, Item::Table(Table::new()));
    }
    let section = doc[table]
        .as_table_like_mut()
        .ok_or_else(|| invalid(format!("`{table}` in the settings file is not a table")))?;
    section.insert(key, toml_edit::value(value));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomically(path, doc.to_string().as_bytes())
}

/// Adds `[accounts.<name>]` with the account's `kind` and, when set, `base_url` to the settings
/// file at `path`, creating the file as needed. An existing table of that name is never
/// overwritten: it is an `AlreadyExists` error.
pub fn add_account(path: &Path, name: &str, account: &Account) -> io::Result<()> {
    let mut doc = read_document(path)?;
    if doc.get("accounts").is_none() {
        let mut accounts = Table::new();
        accounts.set_implicit(true);
        doc.insert("accounts", Item::Table(accounts));
    }
    let accounts = doc["accounts"]
        .as_table_like_mut()
        .ok_or_else(|| invalid("`accounts` in the settings file is not a table"))?;
    if accounts.get(name).is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("`accounts.{name}` is already in the settings file"),
        ));
    }
    let mut table = Table::new();
    table.insert("kind", toml_edit::value(account.kind.as_str()));
    if let Some(url) = account.base_url.as_deref() {
        table.insert("base_url", toml_edit::value(url));
    }
    accounts.insert(name, Item::Table(table));
    write_document(path, &doc)
}

/// Removes `[accounts.<name>]` from the settings file at `path`. `false` when the file has no
/// such table, in which case it is not written.
pub fn remove_account(path: &Path, name: &str) -> io::Result<bool> {
    let mut doc = read_document(path)?;
    let Some(accounts) = doc.get_mut("accounts").and_then(Item::as_table_like_mut) else {
        return Ok(false);
    };
    if accounts.remove(name).is_none() {
        return Ok(false);
    }
    if accounts.is_empty() {
        doc.remove("accounts");
    }
    write_document(path, &doc)?;
    Ok(true)
}

/// The settings key in the file at `path` whose model reference names account `name`, if any.
/// `None` when the file does not define the account, since there is nothing to remove from it.
pub fn model_key_naming(path: &Path, name: &str) -> io::Result<Option<&'static str>> {
    let doc = read_document(path)?;
    if doc.get("accounts").and_then(|a| a.get(name)).is_none() {
        return Ok(None);
    }
    let names = |item: Option<&Item>| {
        item.and_then(Item::as_str)
            .and_then(crate::models::ModelRef::parse)
            .is_some_and(|r| r.account == name)
    };
    let keys: [(&'static str, Option<&Item>); 3] = [
        (
            "[model] model",
            doc.get("model").and_then(|t| t.get("model")),
        ),
        (
            "[guardian] model",
            doc.get("guardian").and_then(|t| t.get("model")),
        ),
        (
            "[guardian.screen] model",
            doc.get("guardian")
                .and_then(|t| t.get("screen"))
                .and_then(|t| t.get("model")),
        ),
    ];
    Ok(keys
        .into_iter()
        .find(|(_, item)| names(*item))
        .map(|(k, _)| k))
}

fn read_document(path: &Path) -> io::Result<DocumentMut> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    parse_document(&text, path)
}

fn write_document(path: &Path, doc: &DocumentMut) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomically(path, doc.to_string().as_bytes())
}

/// Replaces `path` through a same-directory rename so a crash or full disk cannot leave the
/// settings file truncated, which would parse as valid TOML and silently drop its deny rules.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    // Rename onto the link's target, or the link itself would be replaced by a regular file.
    let resolved = std::fs::canonicalize(path).ok();
    let path = resolved.as_deref().unwrap_or(path);
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    if let Ok(meta) = std::fs::metadata(path) {
        std::fs::set_permissions(tmp.path(), meta.permissions())?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_is_added_and_the_rest_of_the_file_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# my settings\n[model]\nmodel = \"a/b\" # keep\n\n[permissions]\ndeny = [\"shell(rm)\"]\n",
        )
        .unwrap();
        append_allow_rule(&path, "read(//tmp/x)").unwrap();
        append_allow_rule(&path, "read(//tmp/x)").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# my settings\n[model]\nmodel = \"a/b\" # keep\n"));
        assert!(text.contains("deny = [\"shell(rm)\"]"));
        assert_eq!(text.matches("read(//tmp/x)").count(), 1);
        assert!(text.contains("allow = [\"read(//tmp/x)\"]"));
    }

    #[test]
    fn set_theme_keeps_comments_and_other_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# mine\n[model]\nmodel = \"a/b\" # keep\n").unwrap();
        set_theme(&path, ThemeName::LightDaltonized).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# mine\n[model]\nmodel = \"a/b\" # keep\n"));
        assert!(text.contains("[tui]\ntheme = \"light-daltonized\""));
        set_theme(&path, ThemeName::Dark).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("theme = \"dark\""));
        assert!(!text.contains("light-daltonized"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_settings_file_stays_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.toml");
        let link = dir.path().join("config.toml");
        std::fs::write(&target, "[model]\nmodel = \"a/b\"\n").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        append_allow_rule(&link, "shell(ls)").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert!(
            std::fs::read_to_string(&target)
                .unwrap()
                .contains("shell(ls)")
        );
    }

    #[test]
    fn an_inline_permissions_table_takes_the_rule() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "permissions = { deny = [\"shell(rm)\"] }\n").unwrap();
        append_allow_rule(&path, "shell(ls)").unwrap();
        let doc: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(doc["permissions"]["allow"][0].as_str(), Some("shell(ls)"));
        assert_eq!(doc["permissions"]["deny"][0].as_str(), Some("shell(rm)"));
    }

    #[test]
    fn the_write_leaves_only_the_settings_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[permissions]\ndeny = [\"shell(rm)\"]\n").unwrap();
        append_allow_rule(&path, "shell(ls)").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("deny = [\"shell(rm)\"]") && text.contains("allow = [\"shell(ls)\"]")
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_missing_file_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        append_allow_rule(&path, "shell(ls)").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[permissions]\nallow = [\"shell(ls)\"]\n"
        );
    }

    #[test]
    fn model_choices_are_written_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# mine\n[model]\nmodel = \"a/old\" # was\n\n[permissions]\ndeny = [\"shell(rm)\"]\n",
        )
        .unwrap();
        set_model(&path, "a/new").unwrap();
        set_guardian_model(&path, "b/judge").unwrap();
        set_guardian_model(&path, "b/judge2").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# mine\n[model]\nmodel = \"a/new\""),
            "{text}"
        );
        assert!(text.contains("deny = [\"shell(rm)\"]"));
        assert!(text.contains("[guardian]\nmodel = \"b/judge2\""), "{text}");
        assert!(!text.contains("b/judge\""));
    }

    #[test]
    fn a_missing_file_gets_the_model_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        set_model(&path, "a/b").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[model]\nmodel = \"a/b\"\n"
        );
    }

    fn account(kind: &str, base_url: Option<&str>) -> Account {
        Account {
            kind: crate::config::AccountKind::named(kind),
            api_key_env: None,
            base_url: base_url.map(str::to_string),
            models: None,
            wires: None,
            context_tokens: None,
            think: None,
            https_proxy: None,
            http_proxy: None,
            no_proxy: None,
            ca_bundle: None,
            client_cert: None,
            client_key: None,
        }
    }

    #[test]
    fn an_account_is_added_and_removed_with_the_rest_of_the_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original =
            "# mine\n[accounts.old] # keep\nkind = \"anthropic\"\n\n[model]\nmodel = \"old/m\"\n";
        std::fs::write(&path, original).unwrap();
        add_account(&path, "local", &account("ollama", Some("http://h:1"))).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# mine\n[accounts.old] # keep\nkind = \"anthropic\"\n"));
        assert!(text.contains("[model]\nmodel = \"old/m\""));
        assert!(
            text.contains("[accounts.local]\nkind = \"ollama\"\nbase_url = \"http://h:1\""),
            "{text}"
        );
        assert!(remove_account(&path, "local").unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn adding_never_overwrites_an_existing_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "[accounts.work]\nkind = \"openai\"\n";
        std::fs::write(&path, original).unwrap();
        let err = add_account(&path, "work", &account("anthropic", None)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn a_missing_file_gets_the_account_and_a_missing_table_is_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        assert!(!remove_account(&path, "work").unwrap());
        assert!(!path.exists());
        add_account(&path, "work", &account("anthropic", None)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[accounts.work]\nkind = \"anthropic\"\n"
        );
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(!remove_account(&path, "other").unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_stays_a_symlink_when_an_account_is_added() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.toml");
        let link = dir.path().join("config.toml");
        std::fs::write(&target, "[model]\nmodel = \"a/b\"\n").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        add_account(&link, "work", &account("openai", None)).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert!(
            std::fs::read_to_string(&target)
                .unwrap()
                .contains("[accounts.work]")
        );
    }

    #[test]
    fn a_bad_rule_or_file_is_refused_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(append_allow_rule(&path, "nonsense(").is_err());
        assert!(!path.exists());
        std::fs::write(&path, "permissions = 3\n").unwrap();
        assert!(append_allow_rule(&path, "shell(ls)").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "permissions = 3\n");
    }
}
