use std::{
    collections::BTreeMap,
    env::VarError,
    io::{self, IsTerminal, Write},
    path::Path,
};

use anyhow::{Context, Result, bail};

// Environment overrides local credentials, including an invalid/empty value:
// never silently fall back to another account when an override is malformed.
pub fn api_key(variable: &str) -> Result<String> {
    resolve_key(
        variable,
        std::env::var(variable),
        &crate::config::directory()?.join("auth.json"),
    )
}

fn resolve_key(
    variable: &str,
    environment: Result<String, VarError>,
    path: &Path,
) -> Result<String> {
    match environment {
        Ok(key) => validate_key(&key).with_context(|| format!("invalid {variable}")),
        Err(VarError::NotUnicode(_)) => bail!("invalid {variable}: expected Unicode"),
        Err(VarError::NotPresent) => {
            let credentials = read_credentials(path)?;
            let key = credentials.get(variable).with_context(|| {
                format!("missing {variable}; set it in the environment or run mdtrans login")
            })?;
            validate_key(key)
                .with_context(|| format!("invalid saved {variable}; run mdtrans login again"))
        }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum LoginProvider {
    Gemini,
    OpenaiCompatible,
}

impl LoginProvider {
    pub fn variable(self) -> &'static str {
        match self {
            Self::Gemini => "GEMINI_API_KEY",
            Self::OpenaiCompatible => "OPENAI_API_KEY",
        }
    }
}

pub fn prompt_credentials(provider: Option<LoginProvider>) -> Result<(LoginProvider, String)> {
    if !io::stdin().is_terminal() {
        bail!(
            "mdtrans login requires an interactive terminal; use an environment variable for automation"
        );
    }
    let provider = match provider {
        Some(provider) => provider,
        None => {
            eprintln!("Provider:\n  1) Gemini\n  2) OpenAI-compatible");
            eprint!("Choose [1/2]: ");
            io::stderr().flush()?;
            let mut selection = String::new();
            io::stdin().read_line(&mut selection)?;
            match selection.trim() {
                "1" | "gemini" => LoginProvider::Gemini,
                "2" | "openai-compatible" => LoginProvider::OpenaiCompatible,
                _ => bail!("choose 1 (gemini) or 2 (openai-compatible)"),
            }
        }
    };
    let path = crate::config::directory()?.join("auth.json");
    eprintln!("The key will be stored unencrypted in {}.", path.display());
    #[cfg(not(unix))]
    eprintln!("Protect this file with your OS user-account permissions (ACLs).");
    let key = rpassword::prompt_password(format!("{} (hidden): ", provider.variable()))
        .context("cannot read API key from terminal")?;
    Ok((provider, validate_key(&key)?))
}

pub fn save(provider: LoginProvider, key: &str) -> Result<()> {
    let variable = provider.variable();
    save_key(
        &crate::config::directory()?.join("auth.json"),
        variable,
        key,
    )?;
    eprintln!("Credential saved locally.");
    if std::env::var_os(variable).is_some() {
        eprintln!(
            "Warning: {variable} is set in the environment and takes precedence over auth.json."
        );
    }
    Ok(())
}

pub fn choose_model(models: &[String]) -> Result<&str> {
    if models.is_empty() {
        bail!(
            "Gemini returned no models supporting generateContent; credentials and config were not changed"
        );
    }
    eprintln!("Models advertising generateContent (availability and quota are not guaranteed):");
    for (index, model) in models.iter().enumerate() {
        eprintln!("  {}) {model}", index + 1);
    }
    eprint!("Select model number (or Ctrl-C to cancel): ");
    io::stderr().flush()?;
    let mut selection = String::new();
    io::stdin().read_line(&mut selection)?;
    let index = selection
        .trim()
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1));
    index
        .and_then(|n| models.get(n))
        .map(String::as_str)
        .context("invalid model selection; credentials and config were not changed")
}

fn validate_key(key: &str) -> Result<String> {
    let key = key.trim();
    if key.is_empty() || key.chars().any(char::is_control) {
        bail!("API key must be nonempty and contain no control characters");
    }
    Ok(key.to_owned())
}

fn read_credentials(path: &Path) -> Result<BTreeMap<String, String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error).context("cannot inspect auth.json"),
    };
    if !metadata.is_file() {
        bail!("auth.json must be a regular file, not a symlink or directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("auth.json is accessible to other users; run chmod 600 on it before continuing");
        }
    }
    let bytes = std::fs::read(path).context("cannot read auth.json")?;
    // Do not include serde's error: malformed JSON may embed the secret in it.
    serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::anyhow!("invalid auth.json; repair or remove it and run mdtrans login")
    })
}

fn save_key(path: &Path, variable: &str, key: &str) -> Result<()> {
    let key = validate_key(key)?;
    let mut credentials = read_credentials(path)?;
    credentials.insert(variable.to_owned(), key);
    let directory = path
        .parent()
        .context("auth.json needs a parent directory")?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(directory)
        .context("cannot create credential directory")?;
    // NamedTempFile is 0600 on Unix; write then atomically replace auth.json.
    let mut file =
        tempfile::NamedTempFile::new_in(directory).context("cannot create credential file")?;
    serde_json::to_writer_pretty(&mut file, &credentials).context("cannot write credentials")?;
    file.write_all(b"\n")?;
    file.as_file()
        .sync_all()
        .context("cannot sync credentials")?;
    // PersistError owns the temporary file; discard it before reporting the error.
    file.persist(path)
        .map_err(|error| error.error)
        .context("cannot save auth.json")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_both_providers_and_replaces_only_selected_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mdtrans/auth.json");
        save_key(&path, "GEMINI_API_KEY", "fake-gemini").unwrap();
        save_key(&path, "OPENAI_API_KEY", "fake-openai").unwrap();
        save_key(&path, "GEMINI_API_KEY", "replacement").unwrap();
        assert_eq!(
            resolve_key("GEMINI_API_KEY", Err(VarError::NotPresent), &path).unwrap(),
            "replacement"
        );
        assert_eq!(
            resolve_key("OPENAI_API_KEY", Err(VarError::NotPresent), &path).unwrap(),
            "fake-openai"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn environment_wins_even_when_saved_file_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        save_key(&path, "GEMINI_API_KEY", "saved").unwrap();
        std::fs::write(&path, "invalid JSON").unwrap();
        assert_eq!(
            resolve_key("GEMINI_API_KEY", Ok("environment".into()), &path).unwrap(),
            "environment"
        );
        assert!(resolve_key("GEMINI_API_KEY", Ok(" ".into()), &path).is_err());
    }

    #[test]
    fn missing_malformed_and_empty_credentials_fail_without_leaking_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        assert!(
            resolve_key("GEMINI_API_KEY", Err(VarError::NotPresent), &path)
                .unwrap_err()
                .to_string()
                .contains("mdtrans login")
        );
        save_key(&path, "OPENAI_API_KEY", "fake-secret").unwrap();
        assert!(save_key(&path, "GEMINI_API_KEY", " ").is_err());
        assert!(save_key(&path, "GEMINI_API_KEY", "a\nb").is_err());
        assert_eq!(read_credentials(&path).unwrap().len(), 1);
        std::fs::write(&path, r#"{"GEMINI_API_KEY": ["fake-secret"]}"#).unwrap();
        let error = resolve_key("GEMINI_API_KEY", Err(VarError::NotPresent), &path).unwrap_err();
        assert!(!format!("{error:#}").contains("fake-secret"));
        assert!(save_key(&path, "GEMINI_API_KEY", "replacement").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_shared_permissions_and_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        save_key(&path, "GEMINI_API_KEY", "fake-key").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_credentials(&path).is_err());
        let link = dir.path().join("link.json");
        symlink(&path, &link).unwrap();
        assert!(save_key(&link, "GEMINI_API_KEY", "replacement").is_err());
    }
}
