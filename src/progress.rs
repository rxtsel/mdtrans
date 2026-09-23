use std::{io::IsTerminal, time::Duration};

use indicatif::{ProgressBar, ProgressStyle};

// Terminal feedback belongs at the CLI edge, not inside translation/providers.
// The guard clears the line on success, early return, or async cancellation.
pub struct Spinner(ProgressBar);

impl Spinner {
    pub fn start(message: &'static str) -> Self {
        if !std::io::stderr().is_terminal() || std::env::var("TERM").as_deref() == Ok("dumb") {
            return Self(ProgressBar::hidden());
        }
        let style = ProgressStyle::with_template("{spinner} {msg} [{elapsed_precise}]")
            .unwrap_or_else(|_| ProgressStyle::default_spinner())
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "]);
        // new_spinner draws to stderr. A dedicated ticker keeps animating while
        // the async HTTP request waits; stdout remains exclusively Markdown.
        let progress = ProgressBar::new_spinner().with_style(style);
        progress.set_message(message);
        progress.enable_steady_tick(Duration::from_millis(80));
        Self(progress)
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.0.finish_and_clear();
    }
}
