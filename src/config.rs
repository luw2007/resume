use std::{env, fs, path::PathBuf, str::FromStr};

use serde::Deserialize;

use crate::cli::Since;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PreviewMode {
    Hidden,
    Visible,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PreviewPosition {
    Auto,
    Right,
    Bottom,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub agents: Option<Vec<String>>,
    pub since: Option<String>,
    pub confirm_always: Option<bool>,
    pub preview: Option<PreviewMode>,
    pub preview_position: Option<PreviewPosition>,
    pub verbose: Option<bool>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("unable to read config {path:?}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config {path:?}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid since value in config {path:?}: {value}")]
    InvalidSince { path: PathBuf, value: String },
}

/// Selects one user-level file only; configuration files are never merged.
pub fn discover_path(
    explicit: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<PathBuf> {
    if explicit.is_some() {
        return explicit;
    }
    if let Some(xdg) = xdg_config_home {
        let path = xdg.join("resume/config.toml");
        if exists(&path) {
            return Some(path);
        }
    }
    home.map(|home| home.join(".config/resume/config.toml"))
        .filter(|path| exists(path))
}

pub fn load(explicit: Option<PathBuf>) -> Result<(Config, Option<PathBuf>), ConfigError> {
    let path = discover_path(
        explicit,
        env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        env::var_os("HOME").map(PathBuf::from),
        std::path::Path::exists,
    );
    match path {
        Some(path) => load_path(path.clone()).map(|config| (config, Some(path))),
        None => Ok((Config::default(), None)),
    }
}

pub fn load_path(path: PathBuf) -> Result<Config, ConfigError> {
    let text = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
        path: path.clone(),
        source,
    })?;
    let config: Config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.clone(),
        source,
    })?;
    if let Some(value) = config.since.as_ref()
        && Since::from_str(value).is_err()
    {
        return Err(ConfigError::InvalidSince {
            path,
            value: value.clone(),
        });
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_then_xdg_then_home_precedence() {
        let explicit = PathBuf::from("/explicit");
        assert_eq!(
            discover_path(
                Some(explicit.clone()),
                Some("/xdg".into()),
                Some("/home".into()),
                |_| true
            ),
            Some(explicit)
        );
        assert_eq!(
            discover_path(None, Some("/xdg".into()), Some("/home".into()), |p| p
                == std::path::Path::new("/xdg/resume/config.toml")),
            Some("/xdg/resume/config.toml".into())
        );
        assert_eq!(
            discover_path(None, Some("/xdg".into()), Some("/home".into()), |p| p
                == std::path::Path::new("/home/.config/resume/config.toml")),
            Some("/home/.config/resume/config.toml".into())
        );
    }

    #[test]
    fn unknown_and_invalid_fields_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [
            ("unknown.toml", "mystery = true"),
            ("position.toml", "preview_position = 'left'"),
            ("preview.toml", "preview = 'sometimes'"),
            ("since.toml", "since = 'yesterday'"),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, body).unwrap();
            assert!(load_path(path).is_err(), "{name}");
        }
    }

    #[test]
    fn documented_field_set_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "agents=['pi']\nsince='7d'\nconfirm_always=true\nverbose=true\npreview='hidden'\npreview_position='bottom'",
        )
        .unwrap();
        let config = load_path(path).unwrap();
        assert_eq!(config.agents, Some(vec!["pi".into()]));
        assert_eq!(config.preview, Some(PreviewMode::Hidden));
        assert_eq!(config.preview_position, Some(PreviewPosition::Bottom));
    }
}
