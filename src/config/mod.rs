use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub default_language: String,
    pub provider: Provider,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Provider {
    OpenaiCompatible { base_url: String, model: String },
    Gemini { model: String },
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read configuration {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid configuration {}", path.display()))
    }

    fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text).context("invalid config.toml")?;
        if config.default_language.trim().is_empty() {
            bail!("default_language must not be empty");
        }
        let model = match &config.provider {
            Provider::OpenaiCompatible { base_url, model } => {
                if base_url.trim().is_empty() {
                    bail!("base_url must not be empty");
                }
                model
            }
            Provider::Gemini { model } => model,
        };
        if model.trim().is_empty() {
            bail!("model must not be empty");
        }
        Ok(config)
    }

    pub fn for_gemini(model: &str) -> Result<Self> {
        Self::gemini_at(&config_path()?, model)
    }

    fn gemini_at(path: &std::path::Path, model: &str) -> Result<Self> {
        let default_language = match std::fs::read_to_string(path) {
            Ok(text) => {
                Self::parse(&text)
                    .context("existing config.toml is invalid; it was not overwritten")?
                    .default_language
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "es".into(),
            Err(error) => return Err(error).context("cannot read existing config.toml"),
        };
        Ok(Self {
            default_language,
            provider: Provider::Gemini {
                model: model.into(),
            },
        })
    }

    pub fn save(&self) -> Result<()> {
        use std::io::Write;
        let directory = directory()?;
        std::fs::create_dir_all(&directory).context("cannot create configuration directory")?;
        let text = toml::to_string_pretty(self).context("cannot serialize configuration")?;
        let mut file = tempfile::NamedTempFile::new_in(&directory)?;
        file.write_all(text.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(directory.join("config.toml"))
            .map_err(|error| error.error)
            .context("cannot save config.toml")?;
        Ok(())
    }

    pub fn language<'a>(&'a self, override_language: Option<&'a str>) -> &'a str {
        override_language.unwrap_or(&self.default_language)
    }
}

fn config_path() -> Result<PathBuf> {
    Ok(directory()?.join("config.toml"))
}

pub fn directory() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("cannot determine the OS configuration directory")?
        .join("mdtrans"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_providers_and_resolves_language() {
        let config = Config::parse("default_language = 'es'\n[provider]\ntype = 'openai-compatible'\nbase_url = 'https://example.com/v1'\nmodel = 'my-model'").unwrap();
        assert_eq!(config.language(None), "es");
        assert_eq!(config.language(Some("fr")), "fr");
        assert!(
            matches!(config.provider, Provider::OpenaiCompatible { base_url, model } if base_url == "https://example.com/v1" && model == "my-model")
        );
        let config = Config::parse(
            "default_language = 'es'\n[provider]\ntype = 'gemini'\nmodel = 'gemini-model'",
        )
        .unwrap();
        assert!(matches!(config.provider, Provider::Gemini { model } if model == "gemini-model"));
    }

    #[test]
    fn gemini_setup_preserves_language_or_defaults_to_spanish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config::gemini_at(&path, "gemini-test").unwrap();
        assert_eq!(config.default_language, "es");
        std::fs::write(&path, "default_language = 'fr'\n[provider]\ntype = 'openai-compatible'\nbase_url = 'https://example.com/v1'\nmodel = 'old'\n").unwrap();
        let config = Config::gemini_at(&path, "gemini-test").unwrap();
        assert_eq!(config.default_language, "fr");
        assert!(matches!(&config.provider, Provider::Gemini { model } if model == "gemini-test"));
        let roundtrip = Config::parse(&toml::to_string_pretty(&config).unwrap()).unwrap();
        assert!(matches!(roundtrip.provider, Provider::Gemini { model } if model == "gemini-test"));
        std::fs::write(&path, "invalid config").unwrap();
        assert!(Config::gemini_at(&path, "gemini-test").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid config");
    }

    #[test]
    fn rejects_invalid_configuration() {
        let valid = "default_language = 'es'\n[provider]\ntype = 'gemini'\nmodel = 'test'";
        for text in [
            valid.replace("'gemini'", "'unknown'"),
            valid.replace("'es'", "' '"),
            valid.replace("'test'", "''"),
            valid.replace("model = 'test'", ""),
            format!("{valid}\napi_key = 'not-allowed'"),
            "broken TOML [".into(),
        ] {
            assert!(Config::parse(&text).is_err());
        }
    }
}
