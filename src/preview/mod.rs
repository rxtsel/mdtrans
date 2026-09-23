use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::Path,
    process::Command,
};

use anyhow::{Context, Result, bail};

pub struct Editor {
    program: String,
    arguments: Vec<String>,
}

impl Editor {
    pub fn from_env() -> Result<Self> {
        Self::parse(
            &std::env::var("EDITOR")
                .context("$EDITOR is missing or invalid; set it to e.g. 'nvim' or 'code --wait'")?,
        )
    }

    fn parse(value: &str) -> Result<Self> {
        let mut words = shell_words::split(value)
            .context("invalid quoting in $EDITOR")?
            .into_iter();
        let program = words
            .next()
            .filter(|word| !word.is_empty())
            .context("$EDITOR must not be empty")?;
        Ok(Self {
            program,
            arguments: words.collect(),
        })
    }

    pub fn open(&self, markdown: &str, source: &Path, language: &str) -> Result<()> {
        // A private, unique directory allows a descriptive filename without
        // collisions, and cleans up editor backup/swap files as well.
        let directory = tempfile::Builder::new()
            .prefix("mdtrans-")
            .tempdir()
            .context("cannot create temporary translation directory")?;
        let path = directory.path().join(translated_filename(source, language));
        {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&path)
                .context("cannot create translated Markdown file")?;
            file.write_all(markdown.as_bytes())
                .context("cannot write translated Markdown file")?;
            file.flush()
                .context("cannot flush translated Markdown file")?;
        }
        // The handle is closed for Windows; the directory guard stays alive
        // until the editor exits. GUI editors must use --wait.
        let status = Command::new(&self.program)
            .args(&self.arguments)
            .arg(&path)
            .status()
            .with_context(|| format!("cannot launch editor {}", self.program))?;
        if !status.success() {
            bail!("editor exited unsuccessfully: {status}");
        }
        Ok(())
    }
}

fn translated_filename(source: &Path, language: &str) -> OsString {
    let mut name = source
        .file_stem()
        .unwrap_or(OsStr::new("document"))
        .to_os_string();
    // Language is also accepted as free text. Never let it create a path or
    // introduce characters forbidden in filenames (e.g. '/' or ':' on Windows).
    let locale: String = language
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    name.push("-");
    name.push(locale);
    name.push("-translate.md");
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_editor_arguments_without_a_shell() {
        let editor = Editor::parse("'my editor' --wait --title 'Markdown preview'").unwrap();
        assert_eq!(editor.program, "my editor");
        assert_eq!(editor.arguments, ["--wait", "--title", "Markdown preview"]);
        for invalid in ["", " ", "''", "nvim '"] {
            assert!(Editor::parse(invalid).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_lives_until_editor_exits_and_source_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        let recorded = dir.path().join("path");
        // sh is explicitly the test editor, not a shell used by the application.
        let editor = Editor {
            program: "sh".into(),
            arguments: vec!["-c".into(), "test -f \"$2\" && test \"$(cat \"$2\")\" = '# Hola' && printf '%s' \"$2\" > \"$1\"".into(), "editor".into(), recorded.to_str().unwrap().into()],
        };
        editor
            .open("# Hola\n", Path::new("source.md"), "es")
            .unwrap();
        let path = std::path::PathBuf::from(std::fs::read_to_string(recorded).unwrap());
        assert_eq!(path.file_name().unwrap(), "source-es-translate.md");
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn descriptive_names_keep_language_inside_temporary_directory() {
        assert_eq!(
            translated_filename(Path::new("docs/README.md"), "pt-BR"),
            "README-pt-BR-translate.md"
        );
        assert_eq!(
            translated_filename(Path::new("docs/Guía.md"), "en"),
            "Guía-en-translate.md"
        );
        let filename = translated_filename(Path::new("README.md"), "../../fr\\test:bad");
        assert_eq!(Path::new(&filename).components().count(), 1);
        assert!(!filename.to_string_lossy().contains(['/', '\\', ':']));
    }

    #[cfg(unix)]
    #[test]
    fn propagates_editor_failure() {
        assert!(
            Editor::parse("sh -c 'exit 7'")
                .unwrap()
                .open("# Hola", Path::new("source.md"), "es")
                .is_err()
        );
    }
}
