use std::ffi::OsString;
use std::path::PathBuf;

use crate::config::ConfigError;

pub const CONFIG_DIR_VAR: &str = "FONTAINE_CONFIG_DIR";
pub const DATA_DIR_VAR: &str = "FONTAINE_DATA_DIR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    pub config: PathBuf,
    pub data: PathBuf,
}

/// Resolves the config and data directories. Overrides come first; otherwise the platform
/// default (supplied by the caller, `None` when the platform has none) plus `fontaine`.
pub fn resolve(
    env: &dyn Fn(&str) -> Option<OsString>,
    default_config: Option<PathBuf>,
    default_data: Option<PathBuf>,
) -> Result<Dirs, ConfigError> {
    Ok(Dirs {
        config: resolve_one(env, CONFIG_DIR_VAR, "config", default_config)?,
        data: resolve_one(env, DATA_DIR_VAR, "data", default_data)?,
    })
}

fn resolve_one(
    env: &dyn Fn(&str) -> Option<OsString>,
    var: &'static str,
    what: &'static str,
    default: Option<PathBuf>,
) -> Result<PathBuf, ConfigError> {
    if let Some(value) = env(var).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    default
        .map(|base| base.join("fontaine"))
        .ok_or(ConfigError::Dir { what, var })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    #[test]
    fn defaults_get_the_fontaine_suffix() {
        let dirs = resolve(
            &no_env,
            Some(PathBuf::from("/cfg")),
            Some(PathBuf::from("/data")),
        )
        .unwrap();
        assert_eq!(dirs.config, PathBuf::from("/cfg/fontaine"));
        assert_eq!(dirs.data, PathBuf::from("/data/fontaine"));
    }

    #[test]
    fn overrides_win_and_are_used_verbatim() {
        let env = |k: &str| match k {
            CONFIG_DIR_VAR => Some(OsString::from("/over/cfg")),
            DATA_DIR_VAR => Some(OsString::from("/over/data")),
            _ => None,
        };
        let dirs = resolve(&env, Some("/cfg".into()), Some("/data".into())).unwrap();
        assert_eq!(dirs.config, PathBuf::from("/over/cfg"));
        assert_eq!(dirs.data, PathBuf::from("/over/data"));
    }

    #[test]
    fn overrides_make_missing_defaults_irrelevant() {
        let env = |k: &str| match k {
            CONFIG_DIR_VAR | DATA_DIR_VAR => Some(OsString::from("/x")),
            _ => None,
        };
        assert!(resolve(&env, None, None).is_ok());
    }

    #[test]
    fn unresolvable_dir_names_the_override_variable() {
        let err = resolve(&no_env, None, Some("/data".into())).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Dir {
                var: CONFIG_DIR_VAR,
                ..
            }
        ));
        assert!(err.to_string().contains(CONFIG_DIR_VAR));

        let err = resolve(&no_env, Some("/cfg".into()), None).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Dir {
                var: DATA_DIR_VAR,
                ..
            }
        ));
        assert!(err.to_string().contains(DATA_DIR_VAR));
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn an_empty_override_counts_as_unset() {
        let env = |k: &str| (k == CONFIG_DIR_VAR).then(OsString::new);
        let err = resolve(&env, None, Some("/data".into())).unwrap_err();
        assert!(matches!(err, ConfigError::Dir { .. }));
    }
}
