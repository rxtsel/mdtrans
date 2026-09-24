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
        Err(error) if broken_pipe(&error) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            cli::report_error(&error);
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
            None,
        ),
    };
    let config = config::Config::load()?;
    let translator = translator(&config.provider)?;
    let language = config.language(lang.as_deref());
    if editor.is_none() && !cli.no_stream {
        let mut stdout = io::stdout();
        let mut emitted = false;
        let mut spinner = Some(progress::Spinner::start(
            "Translating… waiting for first text",
        ));
        let result = translate::stream_file(translator.as_ref(), &path, language, &mut |text| {
            if !text.is_empty() {
                // Clear the spinner before stdout starts; terminal redraws must
                // never erase streamed text or compete with it on the same line.
                spinner.take();
                emitted = true;
                stdout.write_all(text.as_bytes())?;
                stdout.flush()?;
            }
            Ok(())
        })
        .await;
        drop(spinner);
        return result.map_err(|error| {
            if emitted {
                error.context("translation interrupted; partial Markdown was already written to stdout and is incomplete")
            } else {
                error
            }
        });
    }
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

fn broken_pipe(error: &anyhow::Error) -> bool {
    match error.downcast_ref::<translate::TranslationError>() {
        Some(translate::TranslationError::Output(error)) => {
            error.kind() == io::ErrorKind::BrokenPipe
        }
        _ => error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe),
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
