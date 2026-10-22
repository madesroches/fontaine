//! Edits to the user's settings file that Fontaine itself makes. The file is the user's own
//! document, so an edit goes through a format-preserving TOML editor and touches nothing but
//! the one value it adds.

use std::io;
use std::path::Path;

use toml_edit::{Array, DocumentMut, Item, Table, Value};

use crate::permission_rules::parse_rule;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
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
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| invalid(format!("{} is not valid TOML: {e}", path.display())))?;

    if doc.get("permissions").is_none() {
        doc.insert("permissions", Item::Table(Table::new()));
    }
    let permissions = doc["permissions"]
        .as_table_mut()
        .ok_or_else(|| invalid("`permissions` in the settings file is not a table"))?;
    if permissions.get("allow").is_none() {
        permissions.insert("allow", Item::Value(Value::Array(Array::new())));
    }
    let allow = permissions["allow"]
        .as_array_mut()
        .ok_or_else(|| invalid("`permissions.allow` in the settings file is not an array"))?;
    if allow.iter().any(|v| v.as_str() == Some(rule)) {
        return Ok(());
    }
    allow.push(rule);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, doc.to_string())
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
