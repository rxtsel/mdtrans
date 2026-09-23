use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    version,
    about,
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
pub struct Cli {
    /// Markdown file to translate (use ./NAME for files named like subcommands)
    #[arg(required = true)]
    pub file: Option<PathBuf>,
    /// Override the configured target language
    #[arg(long, value_parser = nonempty)]
    pub lang: Option<String>,
    /// Print translated Markdown instead of opening $EDITOR
    #[arg(long)]
    pub stdout: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Alias for the default behavior: translate and open a temporary file in $EDITOR
    Preview(PreviewArgs),
    /// Save an API key locally; discover and select a model for Gemini
    Login {
        /// Omit to choose interactively
        #[arg(value_enum)]
        provider: Option<crate::auth::LoginProvider>,
    },
}

#[derive(Args)]
pub struct PreviewArgs {
    pub file: PathBuf,
    #[arg(long, value_parser = nonempty)]
    pub lang: Option<String>,
}

fn nonempty(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err("language must not be empty".into())
    } else {
        Ok(value.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_translation_and_preview() {
        let cli = Cli::try_parse_from(["mdtrans", "README.md", "--lang", "fr"]).unwrap();
        assert_eq!(cli.file.unwrap(), PathBuf::from("README.md"));
        assert_eq!(cli.lang.as_deref(), Some("fr"));
        assert!(!cli.stdout);
        let cli = Cli::try_parse_from(["mdtrans", "README.md", "--stdout"]).unwrap();
        assert!(cli.stdout);
        assert!(Cli::try_parse_from(["mdtrans", "preview", "README.md", "--stdout"]).is_err());
        let cli = Cli::try_parse_from(["mdtrans", "preview", "README.md", "--lang", "de"]).unwrap();
        let Some(Command::Preview(args)) = cli.command else {
            panic!("expected preview")
        };
        assert_eq!(args.lang.as_deref(), Some("de"));
    }

    #[test]
    fn parses_login_with_optional_provider_without_a_file() {
        for args in [
            vec!["mdtrans", "login"],
            vec!["mdtrans", "login", "gemini"],
            vec!["mdtrans", "login", "openai-compatible"],
        ] {
            assert!(matches!(
                Cli::try_parse_from(args).unwrap().command,
                Some(Command::Login { .. })
            ));
        }
        assert!(Cli::try_parse_from(["mdtrans", "login", "unknown"]).is_err());
    }

    #[test]
    fn rejects_missing_file_and_blank_language() {
        for args in [
            vec!["mdtrans"],
            vec!["mdtrans", "preview"],
            vec!["mdtrans", "README.md", "--lang", " "],
            vec!["mdtrans", "preview", "README.md", "--lang", ""],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
}
