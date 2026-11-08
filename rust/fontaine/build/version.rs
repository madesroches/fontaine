/// The version `--version` and `/bug` show. `sha` is `None` without git information.
pub fn display_version(
    pkg: &str,
    sha: Option<&str>,
    tags_at_head: &[String],
    dirty: bool,
) -> String {
    let Some(sha) = sha else {
        return pkg.to_string();
    };
    let release_tag = format!("v{pkg}");
    if !dirty && tags_at_head.contains(&release_tag) {
        return pkg.to_string();
    }
    let suffix = if dirty { ".dirty" } else { "" };
    format!("{pkg}-dev+{sha}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::display_version;

    fn tags(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn no_git_information_is_the_plain_version() {
        assert_eq!(display_version("0.1.0", None, &[], false), "0.1.0");
        assert_eq!(display_version("0.1.0", None, &[], true), "0.1.0");
    }

    #[test]
    fn the_release_tag_on_a_clean_tree_is_the_plain_version() {
        let t = tags(&["dev-7", "v0.1.0"]);
        assert_eq!(display_version("0.1.0", Some("abc123"), &t, false), "0.1.0");
    }

    #[test]
    fn the_release_tag_on_a_dirty_tree_is_a_dirty_dev_build() {
        let t = tags(&["v0.1.0"]);
        assert_eq!(
            display_version("0.1.0", Some("abc123"), &t, true),
            "0.1.0-dev+abc123.dirty"
        );
    }

    #[test]
    fn no_tag_is_a_dev_build() {
        assert_eq!(
            display_version("0.1.0", Some("abc123"), &[], false),
            "0.1.0-dev+abc123"
        );
    }

    #[test]
    fn a_tag_for_another_version_is_a_dev_build() {
        let t = tags(&["v0.0.9", "dev-3"]);
        assert_eq!(
            display_version("0.1.0", Some("abc123"), &t, false),
            "0.1.0-dev+abc123"
        );
    }

    #[test]
    fn build_metadata_is_valid_semver() {
        for dirty in [false, true] {
            let v = display_version("0.1.0", Some("abc123def456"), &[], dirty);
            let (_, meta) = v.split_once('+').expect("dev builds carry metadata");
            assert!(!meta.is_empty());
            assert!(
                meta.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
            );
        }
    }
}
