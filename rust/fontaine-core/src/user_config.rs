//! Edits to the user's settings file that Fontaine itself makes. The file is the user's own
//! document, so an edit goes through a format-preserving TOML editor and touches nothing but
//! the one value it adds.

use std::io;
use std::path::Path;

use toml_edit::{Array, DocumentMut, Item, Table, Value};

use crate::config::{Account, Column, ThemeName};
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
    let _lock = locked(path)?;
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

    crate::owner_only::write_atomic_keeping_mode(path, doc.to_string().as_bytes())
}

/// Sets `[model] model` in the settings file at `path` to `reference` (`<account>/<model>`).
pub fn set_model(path: &Path, reference: &str) -> io::Result<()> {
    set_value(path, &["model", "model"], Some(reference.into())).map(|_| ())
}

/// Sets `[guardian] model` in the settings file at `path` to `reference` (`<account>/<model>`).
pub fn set_guardian_model(path: &Path, reference: &str) -> io::Result<()> {
    set_value(path, &["guardian", "model"], Some(reference.into())).map(|_| ())
}

/// Sets `[tui] theme` in the settings file at `path`.
pub fn set_theme(path: &Path, name: ThemeName) -> io::Result<()> {
    set_value(path, &["tui", "theme"], Some(name.as_str().into())).map(|_| ())
}

/// Sets or, for `None`, removes the setting at the dotted `key` (`tui.theme`) in the settings
/// file at `path`. `false` means a removal found nothing to remove.
pub fn set_config_value(path: &Path, key: &str, value: Option<Value>) -> io::Result<bool> {
    let segments: Vec<&str> = key.split('.').collect();
    set_value(path, &segments, value)
}

/// Sets the key at `key_path` to `value`, or removes it when `None` (dropping any table the
/// removal leaves empty), creating the file and the tables as needed and touching nothing else
/// in the file. An edit that would leave the file unloadable is refused and nothing is written.
pub fn set_value(path: &Path, key_path: &[&str], value: Option<Value>) -> io::Result<bool> {
    let (leaf, parents) = key_path
        .split_last()
        .expect("a settings key path has at least one segment");
    let _lock = locked(path)?;
    let mut doc = read_document(path, |p| std::fs::read_to_string(p))?;
    match value {
        Some(value) => {
            let mut table = doc.as_table_mut() as &mut dyn toml_edit::TableLike;
            for segment in parents {
                if table.get(segment).is_none() {
                    table.insert(segment, Item::Table(Table::new()));
                }
                table = table
                    .get_mut(segment)
                    .and_then(Item::as_table_like_mut)
                    .ok_or_else(|| {
                        invalid(format!("`{segment}` in the settings file is not a table"))
                    })?;
            }
            table.insert(leaf, Item::Value(value));
        }
        None => {
            if !remove_key(doc.as_table_mut(), parents, leaf) {
                return Ok(false);
            }
        }
    }
    crate::config::validate_settings(&doc.to_string(), path, Column::User)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_document(path, &doc)?;
    Ok(true)
}

/// Replaces the table at `key_path` with `table` in one edit, creating the file and the parent
/// tables as needed and touching nothing else in the file. An edit that would leave the file
/// unloadable is refused and nothing is written. Several `set_value` calls could leave a
/// half-written section between them; this cannot.
pub fn set_table(path: &Path, key_path: &[&str], table: Table) -> io::Result<()> {
    let (leaf, parents) = key_path
        .split_last()
        .expect("a settings key path has at least one segment");
    let _lock = locked(path)?;
    let mut doc = read_document(path, |p| std::fs::read_to_string(p))?;
    let mut parent = doc.as_table_mut() as &mut dyn toml_edit::TableLike;
    for segment in parents {
        if parent.get(segment).is_none() {
            let mut intermediate = Table::new();
            intermediate.set_implicit(true);
            parent.insert(segment, Item::Table(intermediate));
        }
        parent = parent
            .get_mut(segment)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| invalid(format!("`{segment}` in the settings file is not a table")))?;
    }
    parent.insert(leaf, Item::Table(table));
    crate::config::validate_settings(&doc.to_string(), path, Column::User)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_document(path, &doc)
}

/// Removes `parents.leaf` and every table the removal empties; `false` when the key is not there.
fn remove_key(table: &mut dyn toml_edit::TableLike, parents: &[&str], leaf: &str) -> bool {
    let Some((first, rest)) = parents.split_first() else {
        return table.remove(leaf).is_some();
    };
    let Some(child) = table.get_mut(first).and_then(Item::as_table_like_mut) else {
        return false;
    };
    let removed = remove_key(child, rest, leaf);
    if removed && child.is_empty() {
        table.remove(first);
    }
    removed
}

/// Adds `[accounts.<name>]` with the account's `kind` and, when set, `base_url` to the settings
/// file at `path`, creating the file as needed. An existing table of that name is never
/// overwritten: it is an `AlreadyExists` error.
pub fn add_account(path: &Path, name: &str, account: &Account) -> io::Result<()> {
    let _lock = locked(path)?;
    let mut doc = read_document(path, |p| std::fs::read_to_string(p))?;
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
    let _lock = locked(path)?;
    let mut doc = read_document(path, |p| std::fs::read_to_string(p))?;
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
    let doc = read_document(path, crate::file_lock::read_shared)?;
    if doc.get("accounts").and_then(|a| a.get(name)).is_none() {
        return Ok(None);
    }
    let names = |item: Option<&Item>| {
        item.and_then(Item::as_str)
            .and_then(crate::models::ModelRef::parse)
            .is_some_and(|r| r.account == name)
    };
    let composition = doc.get("guardian").and_then(|t| t.get("composition"));
    let mut keys: Vec<(&'static str, Option<&Item>)> = vec![
        (
            "[model] model",
            doc.get("model").and_then(|t| t.get("model")),
        ),
        (
            "[guardian] model",
            doc.get("guardian").and_then(|t| t.get("model")),
        ),
        (
            "[guardian.composition] model",
            composition.and_then(|t| t.get("model")),
        ),
    ];
    if let Some(reads) = composition
        .and_then(|t| t.get("reads"))
        .and_then(Item::as_table_like)
    {
        keys.extend(
            reads
                .iter()
                .map(|(_, read)| ("[guardian.composition.reads] model", read.get("model"))),
        );
    }
    Ok(keys
        .into_iter()
        .find(|(_, item)| names(*item))
        .map(|(k, _)| k))
}

/// Serializes read-modify-write of the settings file across sessions; held until the edit is
/// written.
fn locked(path: &Path) -> io::Result<crate::file_lock::FileLock> {
    crate::file_lock::lock(path)
}

fn read_document(path: &Path, read: fn(&Path) -> io::Result<String>) -> io::Result<DocumentMut> {
    let text = match read(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    parse_document(&text, path)
}

fn write_document(path: &Path, doc: &DocumentMut) -> io::Result<()> {
    crate::owner_only::write_atomic_keeping_mode(path, doc.to_string().as_bytes())
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

    #[test]
    fn set_config_value_replaces_and_removes_keeping_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# mine\n[model]\nmodel = \"a/b\" # keep\n\n[context]\nautocompact_window = 1000\n",
        )
        .unwrap();
        set_config_value(&path, "context.autocompact_window", Some(2000.into())).unwrap();
        set_config_value(&path, "schedule.enabled", Some(false.into())).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# mine\n[model]\nmodel = \"a/b\" # keep\n"),
            "{text}"
        );
        assert!(text.contains("autocompact_window = 2000"), "{text}");
        assert!(text.contains("[schedule]\nenabled = false"), "{text}");

        set_config_value(&path, "schedule.enabled", None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("schedule"),
            "an emptied table is dropped: {text}"
        );
        assert!(text.contains("model = \"a/b\" # keep"));
    }

    #[test]
    fn removing_an_absent_key_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(!set_config_value(&path, "tui.theme", None).unwrap());
        assert!(!path.exists());
        std::fs::write(&path, "[model]\nmodel = \"a/b\"\n").unwrap();
        assert!(!set_config_value(&path, "tui.theme", None).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[model]\nmodel = \"a/b\"\n"
        );
    }

    #[test]
    fn an_edit_that_would_break_the_file_is_refused_and_leaves_it_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "[model]\nmodel = \"a/b\"\n";
        std::fs::write(&path, original).unwrap();
        for (key, value) in [
            ("context.autocompact_window", toml_edit::Value::from(0)),
            ("tui.theme", toml_edit::Value::from("not-a-theme")),
        ] {
            let err = set_config_value(&path, key, Some(value)).unwrap_err();
            assert!(
                err.get_ref()
                    .is_some_and(|inner| inner.is::<crate::config::ConfigError>()),
                "{err}"
            );
            assert!(!err.to_string().is_empty());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
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
        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["config.lock", "config.toml"]);
    }

    #[test]
    fn concurrent_edits_of_one_file_all_land() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[permissions]\ndeny = [\"shell(rm)\"]\n").unwrap();
        let rules: Vec<String> = (0..8).map(|i| format!("shell(cmd{i})")).collect();
        std::thread::scope(|scope| {
            for rule in &rules {
                let path = &path;
                scope.spawn(move || append_allow_rule(path, rule).unwrap());
            }
        });
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("deny = [\"shell(rm)\"]"), "{text}");
        for rule in &rules {
            assert!(text.contains(rule.as_str()), "{rule} missing from {text}");
        }
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
            prompt_caching: None,
            cache_ttl: None,
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
