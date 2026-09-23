mod auth;
mod cli;
mod config;
mod preview;
mod progress;
mod providers;
mod translate;

use std::io::{self, Write};

use anyhow::{Context, Result};
use clap::Parser;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mdtrans: {error:#}");
            if let Some(translate::TranslationError::Http(status)) = error.downcast_ref() {
                let hint = match status {
                    401 => "API key rejected; check your environment or run mdtrans login.",
                    403 => "Access denied; check API key permissions and project access.",
                    404 => {
                        "Model or API endpoint not found/available. Check config.toml; for Gemini, run mdtrans login gemini to discover models."
                    }
                    429 => {
                        "Rate limit or quota exceeded; check provider quota/billing and retry later."
                    }
                    503 => {
                        "Provider temporarily unavailable or overloaded. Try again later; no automatic retry was made."
                    }
                    _ => "Check provider availability and configuration.",
                };
                eprintln!("Hint: {hint}");
            }
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let cli = cli::Cli::parse();
    let (path, lang, editor) = match cli.command {
        Some(cli::Command::Login { provider }) => return login(provider).await,
        Some(cli::Command::Preview(args)) => {
            (args.file, args.lang, Some(preview::Editor::from_env()?))
        }
        None => (
            cli.file.context("a Markdown file is required")?,
            cli.lang,
            if cli.stdout {
                None
            } else {
                Some(preview::Editor::from_env()?)
            },
        ),
    };
    let config = config::Config::load()?;
    let translator = translator(&config.provider)?;
    let language = config.language(lang.as_deref());
    let markdown = {
        let _spinner = progress::Spinner::start("Translating… waiting for provider");
        translate::translate_file(translator.as_ref(), &path, language).await?
    };
    if let Some(editor) = editor {
        return editor.open(&markdown, &path, language);
    }
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(markdown.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result.context("cannot write translated Markdown to stdout"),
    }
}

async fn login(provider: Option<auth::LoginProvider>) -> Result<()> {
    let (provider, key) = auth::prompt_credentials(provider)?;
    match provider {
        auth::LoginProvider::Gemini => {
            let models = {
                let _spinner = progress::Spinner::start("Discovering Gemini models…");
                providers::gemini::list_models(&providers::client()?, &key).await?
            };
            eprintln!(
                "Selecting a model will set provider=gemini in config.toml and preserve default_language (es for new configs)."
            );
            let model = auth::choose_model(&models)?;
            let config = config::Config::for_gemini(model)?;
            auth::save(provider, &key)?;
            config
                .save()
                .context("credential saved, but config.toml could not be updated")?;
            eprintln!(
                "Configured Gemini model: {model}. Listing succeeded; translation access/quota is not guaranteed."
            );
        }
        auth::LoginProvider::OpenaiCompatible => {
            auth::save(provider, &key)?;
            eprintln!(
                "Key not validated remotely. Set provider, base_url and model in config.toml."
            );
        }
    }
    Ok(())
}

fn translator(provider: &config::Provider) -> Result<Box<dyn translate::Translator>> {
    let client = providers::client().context("cannot initialize HTTP client")?;
    match provider {
        config::Provider::OpenaiCompatible { base_url, model } => Ok(Box::new(
            providers::openai_compatible::OpenAiCompatible::new(
                client,
                base_url,
                model.clone(),
                &auth::api_key("OPENAI_API_KEY")?,
            )?,
        )),
        config::Provider::Gemini { model } => Ok(Box::new(providers::gemini::Gemini::new(
            client,
            model,
            &auth::api_key("GEMINI_API_KEY")?,
        )?)),
    }
}
