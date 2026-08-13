use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::cli::SUPPORTED_AGENTS;

const SCHEMA_VERSION: u32 = 1;
const SETTINGS_RELATIVE_PATH: &str = ".resume/settings.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    schema_version: u32,
    agents: Vec<String>,
    known_agents: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("HOME is not set; cannot locate ~/.resume/settings.json")]
    HomeUnavailable,
    #[error("unable to read settings {path:?}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid settings {path:?}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("unsupported settings schema version {0}")]
    UnsupportedSchema(u32),
    #[error("settings contain unsupported agent {0:?}")]
    UnknownAgent(String),
    #[error("unable to write settings {path:?}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("no controlling terminal available; run `resume setup` in an interactive terminal")]
    NoTerminal,
    #[error("invalid agent selection {0:?}; use comma-separated numbers, `all`, or `none`")]
    InvalidSelection(String),
}

impl Settings {
    pub fn agents(&self) -> &[String] {
        &self.agents
    }

    fn validate(&self) -> Result<(), SettingsError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(SettingsError::UnsupportedSchema(self.schema_version));
        }
        for agent in self.agents.iter().chain(&self.known_agents) {
            if !SUPPORTED_AGENTS.contains(&agent.as_str()) {
                return Err(SettingsError::UnknownAgent(agent.clone()));
            }
        }
        Ok(())
    }
}

impl Default for Settings {
    fn default() -> Self {
        current_settings(SUPPORTED_AGENTS.iter().map(ToString::to_string).collect())
    }
}

pub fn settings_path(home: &Path) -> PathBuf {
    home.join(SETTINGS_RELATIVE_PATH)
}

fn home_dir() -> Result<PathBuf, SettingsError> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(SettingsError::HomeUnavailable)
}

pub fn load(home: &Path) -> Result<Option<Settings>, SettingsError> {
    let path = settings_path(home);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(SettingsError::Read { path, source }),
    };
    let settings: Settings =
        serde_json::from_str(&text).map_err(|source| SettingsError::Parse { path, source })?;
    settings.validate()?;
    Ok(Some(settings))
}

fn current_settings(agents: Vec<String>) -> Settings {
    Settings {
        schema_version: SCHEMA_VERSION,
        agents,
        known_agents: SUPPORTED_AGENTS.iter().map(ToString::to_string).collect(),
    }
}

pub fn save(home: &Path, settings: &Settings) -> Result<(), SettingsError> {
    settings.validate()?;
    let path = settings_path(home);
    let parent = path.parent().expect("settings path has a parent");
    fs::create_dir_all(parent).map_err(|source| SettingsError::Write {
        path: parent.to_path_buf(),
        source,
    })?;
    let temporary = parent.join(format!(".settings-{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(settings).expect("Settings always serializes");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|source| SettingsError::Write {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(&bytes)
        .map_err(|source| SettingsError::Write {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(b"\n")
        .map_err(|source| SettingsError::Write {
            path: temporary.clone(),
            source,
        })?;
    file.sync_all().map_err(|source| SettingsError::Write {
        path: temporary.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).map_err(|source| {
            SettingsError::Write {
                path: temporary.clone(),
                source,
            }
        })?;
    }
    fs::rename(&temporary, &path).map_err(|source| SettingsError::Write { path, source })
}

pub fn newly_supported(settings: &Settings) -> Vec<&'static str> {
    SUPPORTED_AGENTS
        .iter()
        .copied()
        .filter(|agent| !settings.known_agents.iter().any(|known| known == agent))
        .collect()
}

pub fn refresh_known_agents(
    home: &Path,
    mut settings: Settings,
) -> Result<(Settings, Vec<&'static str>), SettingsError> {
    let new_agents = newly_supported(&settings);
    if !new_agents.is_empty() {
        settings.known_agents = SUPPORTED_AGENTS.iter().map(ToString::to_string).collect();
        save(home, &settings)?;
    }
    Ok((settings, new_agents))
}

pub fn parse_selection(input: &str) -> Result<Vec<String>, SettingsError> {
    let input = input.trim();
    if input.eq_ignore_ascii_case("all") {
        return Ok(SUPPORTED_AGENTS.iter().map(ToString::to_string).collect());
    }
    if input.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    if input.is_empty() {
        return Err(SettingsError::InvalidSelection(input.into()));
    }
    let mut selected = Vec::new();
    for part in input.split(',') {
        let number = part
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|number| (1..=SUPPORTED_AGENTS.len()).contains(number))
            .ok_or_else(|| SettingsError::InvalidSelection(input.into()))?;
        let agent = SUPPORTED_AGENTS[number - 1].to_string();
        if !selected.contains(&agent) {
            selected.push(agent);
        }
    }
    Ok(selected)
}

fn select_agents(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<Vec<String>, SettingsError> {
    writeln!(output, "Choose agents to scan:").map_err(|source| SettingsError::Write {
        path: PathBuf::from("/dev/tty"),
        source,
    })?;
    for (index, agent) in SUPPORTED_AGENTS.iter().enumerate() {
        writeln!(output, "  {}. {agent}", index + 1).map_err(|source| SettingsError::Write {
            path: PathBuf::from("/dev/tty"),
            source,
        })?;
    }
    write!(output, "Selection (for example 1,3; `all`; or `none`): ").map_err(|source| {
        SettingsError::Write {
            path: PathBuf::from("/dev/tty"),
            source,
        }
    })?;
    output.flush().map_err(|source| SettingsError::Write {
        path: PathBuf::from("/dev/tty"),
        source,
    })?;
    let mut line = String::new();
    input
        .read_line(&mut line)
        .map_err(|source| SettingsError::Read {
            path: PathBuf::from("/dev/tty"),
            source,
        })?;
    parse_selection(&line)
}

pub fn run_setup() -> Result<Settings, SettingsError> {
    let tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| SettingsError::NoTerminal)?;
    let mut output = tty.try_clone().map_err(|source| SettingsError::Write {
        path: PathBuf::from("/dev/tty"),
        source,
    })?;
    let mut input = BufReader::new(tty);
    let agents = select_agents(&mut input, &mut output)?;
    let home = home_dir()?;
    let settings = current_settings(agents);
    save(&home, &settings)?;
    writeln!(output, "Saved {}", settings_path(&home).display()).map_err(|source| {
        SettingsError::Write {
            path: PathBuf::from("/dev/tty"),
            source,
        }
    })?;
    Ok(settings)
}

pub fn load_or_setup() -> Result<(Settings, Vec<&'static str>), SettingsError> {
    let home = home_dir()?;
    match load(&home)? {
        Some(settings) => refresh_known_agents(&home, settings),
        None => Ok((run_setup()?, Vec::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_file_loads_as_none() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(load(home.path()).unwrap(), None);
    }

    #[test]
    fn roundtrip_preserves_selection_and_known_agents() {
        let home = tempfile::tempdir().unwrap();
        let settings = current_settings(vec!["pi".into()]);
        save(home.path(), &settings).unwrap();
        assert_eq!(load(home.path()).unwrap(), Some(settings));
    }

    #[test]
    fn invalid_settings_are_rejected() {
        let home = tempfile::tempdir().unwrap();
        let path = settings_path(home.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for body in [
            "not json",
            r#"{"schema_version":0,"agents":[],"known_agents":[]}"#,
            r#"{"schema_version":1,"agents":["unknown"],"known_agents":[]}"#,
        ] {
            fs::write(&path, body).unwrap();
            assert!(load(home.path()).is_err(), "{body}");
        }
    }

    #[test]
    fn parse_selection_accepts_numbers_all_and_none() {
        assert_eq!(parse_selection("2, 1, 2").unwrap(), ["claude", "codex"]);
        assert_eq!(parse_selection("all").unwrap(), SUPPORTED_AGENTS);
        assert!(parse_selection("none").unwrap().is_empty());
        assert!(parse_selection("").is_err());
    }

    #[test]
    fn new_agents_are_reported_and_not_selected() {
        let home = tempfile::tempdir().unwrap();
        let settings = Settings {
            schema_version: SCHEMA_VERSION,
            agents: vec!["pi".into()],
            known_agents: vec!["codex".into(), "claude".into(), "pi".into(), "omp".into()],
        };
        let (updated, new_agents) = refresh_known_agents(home.path(), settings).unwrap();
        assert_eq!(new_agents, ["opencode"]);
        assert_eq!(updated.agents(), ["pi"]);
        assert_eq!(updated.known_agents, SUPPORTED_AGENTS);

        let (_, repeated) = refresh_known_agents(home.path(), updated).unwrap();
        assert!(repeated.is_empty());
    }
}
