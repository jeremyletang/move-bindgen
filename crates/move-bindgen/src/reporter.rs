//! Cargo-style status reporter for `install` / `generate`.
//!
//! Output shape mirrors cargo's: a 12-char right-aligned verb (bold-green
//! when stderr is a TTY) followed by a single-line subject. Lives on
//! stderr so stdout stays clean for piping.
//!
//! ```text
//!   Resolving configs/exchange.toml
//!    Fetching pyth (https://github.com/.../pyth-crosschain.git@iota-contract-testnet)
//!     Staging real_markets
//!    Compiling real_markets
//!   Generating exchange-rs (15 crates)
//!    Finished installing 15 packages in 1.2s
//! ```
//!
//! `Reporter::quiet()` silences everything; the CLI exposes `--quiet`
//! for scripting use. Errors propagate via `Result` regardless.
//!
//! Deliberately simple — no global state, no progress bars, no spinner.
//! If we want richer output later (e.g. parallel-build progress),
//! revisit then.
use std::io::{self, IsTerminal, Write};

const VERB_WIDTH: usize = 12;
const GREEN_BOLD: &str = "\x1b[1;32m";
const RESET: &str = "\x1b[0m";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReporterMode {
    Normal,
    Quiet,
}

#[derive(Debug, Clone)]
pub struct Reporter {
    mode: ReporterMode,
    color: bool,
}

impl Reporter {
    /// Standard reporter — chatty, colorised when stderr is a TTY.
    pub fn new() -> Self {
        Self {
            mode: ReporterMode::Normal,
            color: io::stderr().is_terminal(),
        }
    }

    /// Suppress all status output. Errors still propagate via `Result`.
    pub fn quiet() -> Self {
        Self {
            mode: ReporterMode::Quiet,
            color: false,
        }
    }

    /// Print a `<verb> <subject>` status line.
    pub fn stage(&self, verb: &str, subject: impl AsRef<str>) {
        if self.mode == ReporterMode::Quiet {
            return;
        }
        let pad = VERB_WIDTH.saturating_sub(verb.len());
        let mut err = io::stderr().lock();
        if self.color {
            let _ = writeln!(
                err,
                "{:pad$}{GREEN_BOLD}{verb}{RESET} {}",
                "",
                subject.as_ref()
            );
        } else {
            let _ = writeln!(err, "{:pad$}{verb} {}", "", subject.as_ref());
        }
    }
}

impl Default for Reporter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_is_quiet() {
        let r = Reporter::quiet();
        // Nothing to assert beyond "doesn't panic" — Reporter writes to
        // process stderr and we don't capture it. The branch matters
        // for coverage of the early-return path.
        r.stage("Verb", "subject");
    }

    #[test]
    fn padding_layout_is_12_chars() {
        // Padding is computed as (12 - verb.len()) spaces; verbs longer
        // than 12 chars emit no leading space (saturating_sub).
        for verb in &[
            "Resolving",
            "Fetching",
            "Staging",
            "Compiling",
            "Generating",
            "Finished",
            "Verifying",
        ] {
            let pad = VERB_WIDTH.saturating_sub(verb.len());
            assert_eq!(pad + verb.len(), VERB_WIDTH, "alignment broken for {verb}");
        }
    }
}
