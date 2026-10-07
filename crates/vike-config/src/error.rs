//! [`ConfigError`] — every failure names the FILE and the KEY.
//!
//! The bar this type exists to clear: `"invalid config"` is useless to the person holding the
//! settings. ``"settings database (section `policy`): market_slippage = 0.9 exceeds the allowed
//! maximum 0.05"`` names the store and section to look in, the key to edit, the value that was
//! rejected AND the bound it broke — four facts, no grep. Every variant below is shaped to carry
//! that much, which is why there is no
//! catch-all `Other(String)`: a variant that cannot name a file is a variant that produces the
//! first message.
//!
//! Env and CLI failures name the VARIABLE/FLAG instead of a file, for the same reason — there is
//! no file to open, so the message points at the shell.

use std::fmt;
use std::path::PathBuf;

/// A settings load failure. Every variant names either a (file, key) pair or the env
/// var / CLI flag that carried the bad value.
#[derive(Debug)]
pub enum ConfigError {
    /// The file exists but could not be read (permissions, a directory in its place, I/O).
    /// An ABSENT file is NOT an error — a missing layer is simply skipped (see `crate::load`).
    Read {
        /// The file that could not be read.
        file: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// The file is not valid TOML, or does not match the schema (an unknown key, a wrong type).
    /// `key` is extracted from the parser's own message when it names one — an unknown/missing
    /// field always does; a pure syntax error does not, and then the message carries line/column
    /// instead.
    Parse {
        /// The file that failed to parse.
        file: PathBuf,
        /// The offending key, when the parser named one.
        key: Option<String>,
        /// The parser's message, verbatim.
        message: String,
    },
    /// The file parsed, but a value is out of range or otherwise nonsense. This is the variant
    /// that produces the canonical message shape:
    /// ``settings database (section `policy`): market_slippage = 0.9 exceeds the allowed maximum
    /// 0.05``.
    Value {
        /// The file the value came from.
        file: PathBuf,
        /// Dotted key path within that file, e.g. `market_slippage`.
        key: String,
        /// The rendered value followed by the reason, e.g. `1.5 exceeds the allowed maximum 0.95`.
        message: String,
    },
    /// An environment variable carried an unparseable or out-of-range value. Unlike today's
    /// scattered readers, an unrecognized value is an ERROR rather than a silent fallback — see
    /// [`crate::flags`] for why a typo'd toggle must not be indistinguishable from "off".
    Env {
        /// The variable name.
        var: String,
        /// Its raw value.
        value: String,
        /// Why it was rejected.
        message: String,
    },
    /// A CLI override carried an unparseable or out-of-range value.
    Cli {
        /// The flag name, without leading dashes.
        flag: String,
        /// Its raw value.
        value: String,
        /// Why it was rejected.
        message: String,
    },
    /// A `<project>/vike.toml` could not be ruled out, and the per-project override layer it
    /// belonged to has been REMOVED. See [`crate::removed`] for why this refuses instead of
    /// ignoring the file, and [`crate::removed::REMOVED_PROJECT_FILE`] for the file name.
    ///
    /// It names a file like every other variant here, which is exactly why it is a variant rather
    /// than a `String`: the thing to fix is a file on disk, and the message says which one and
    /// where its two tables go.
    ///
    /// ⚠ "could not be ruled out" rather than "is present", and that is
    /// [`probe`](crate::removed::RemovedFileProbe)'s
    /// whole job — the refusal is the same either way, the CLAIM is not.
    RemovedProjectFile {
        /// The offending path, in full — an operator asking "which one?" gets a path, not a guess.
        file: PathBuf,
        /// Whether the file was actually SEEN, or the path could not be probed at all. Only the
        /// first may say "is present"; see [`crate::removed::RemovedFileProbe`].
        probe: crate::removed::RemovedFileProbe,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Read { file, source } => {
                write!(f, "{}: cannot be read: {source}", file.display())
            }
            ConfigError::Parse { file, key: Some(key), message } => {
                write!(f, "{}: {key}: {message}", file.display())
            }
            ConfigError::Parse { file, key: None, message } => {
                write!(f, "{}: {message}", file.display())
            }
            ConfigError::Value { file, key, message } => {
                write!(f, "{}: {key} = {message}", file.display())
            }
            ConfigError::Env { var, value, message } => write!(f, "{var}={value}: {message}"),
            ConfigError::Cli { flag, value, message } => {
                write!(f, "--{flag} {value}: {message}")
            }
            ConfigError::RemovedProjectFile { file, probe } => {
                write!(f, "{}", crate::removed::removed_project_file_message(file, probe))
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Read { source, .. } => Some(source),
            // The stat that could not answer. Carried through so a caller can match on the errno
            // rather than on the prose — `EACCES` under `ProtectHome=` is a different problem from
            // an `EIO` on a dying disk, and the message says so in words for the human.
            ConfigError::RemovedProjectFile {
                probe: crate::removed::RemovedFileProbe::Unestablished(e),
                ..
            } => Some(e),
            _ => None,
        }
    }
}

/// **What a TOMBSTONE refusal says about getting rid of the stale row** — the one sentence the three
/// tombstone arms (`Policy::apply`'s `max_total_exposure` and `rate.max_utilization`,
/// `Preferences::apply`'s `rate_utilization`) share, so they cannot drift.
///
/// ⚠ **"Remove this row from the settings database itself" would be a trap.** There is no verb
/// that deletes a settings row (no `config
/// unset`: `crate::remedy`'s `unset_clause` writes the default back through `config set`, and a
/// removed key has no default to write). A hand `DELETE` changes the `setting` row count that
/// `settings_adoption.setting_rows` sealed, and [`crate::mirror`]'s `adoption_integrity` then
/// refuses the next boot as rows that "left by some other route"; the 0086 repair is restore-from-
/// backup only, and a backup still holds the tombstone row. So the true state is that nothing
/// sanctioned removes such a row yet, and the message says that instead of advising a swap of one
/// boot refusal for another. A verb that removes a row AND keeps the seal's counts honest is owed.
pub(crate) const STALE_ROW_REMOVAL: &str = "There is no sanctioned way to remove this row yet: \
     no verb deletes a settings row (there is no `config unset`; it is owed), and on a sealed \
     store a hand DELETE changes the row count the seal holds, so the next boot refuses it as a \
     seal refusal. Until that verb exists this row keeps the store from loading";

impl ConfigError {
    /// Build a [`ConfigError::Value`] for a numeric field, rendering the value into the message
    /// so the caller only supplies the REASON. Keeps every out-of-range message the same shape.
    pub(crate) fn value(file: &std::path::Path, key: &str, value: f64, reason: &str) -> Self {
        ConfigError::Value {
            file: file.to_path_buf(),
            key: key.to_string(),
            message: format!("{value} {reason}"),
        }
    }
}

/// Render a `toml` deserialize failure into a message that CANNOT carry the offending source line.
///
/// ⚠ This is the redaction, and it is structural rather than a filter. `toml::de::Error`'s own
/// `Display` renders an ANNOTATED SNIPPET — the file's own text, verbatim, under an `N | ` gutter —
/// whenever it holds both the input and a span. A TOML document read from disk (a run profile
/// today; the settings TOMLs before decision 0086) can sit in the same directory as `secrets.env`,
/// so `api_key = "…"` written into one is a plausible first-time mistake;
/// `deny_unknown_fields` correctly refuses the key, and the refusal then echoed the whole line,
/// value included, onto the stderr of every `vike-cli` invocation — the exact "paste this into a
/// bug report" path.
///
/// Dropping the INPUT is what removes it: the snippet arm is `if let (Some(input), Some(span))`, so
/// with no input there is no branch that can print file text at all. Post-hoc scrubbing of the
/// rendered string would be a filter over a shape the FILE controls, and a filter is only ever as
/// good as its last update; this removes the capability instead.
///
/// The LOCATION survives, because it is the useful half and it is content-free: it is computed here
/// from the span, which is a pair of integers. The key survives too — an unknown-field message names
/// it, and [`key_from_parse_message`] still finds it in the result.
///
/// The rule this draws, stated so it can be applied to a new field: **a settings error may render a
/// value for a key `vike-config` OWNS (a malformed address, an out-of-range utilization — the
/// operator cannot fix it without being told what it rejected), and never for a key it does not.**
/// An unknown key is by definition the second case.
pub(crate) fn redacted_parse_message(text: &str, mut error: toml::de::Error) -> String {
    let at = error.span().map(|s| line_col(text, s.start));
    // ⚠ LOAD-BEARING: without this the next line renders the offending source line verbatim.
    error.set_input(None);
    // `Display` writes each part with `writeln!`, so the result is multi-line and trailing-newlined;
    // a `ConfigError` is one sentence, so the parts are joined rather than reflowed.
    let message = error
        .to_string()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    match at {
        Some((line, column)) => format!("at line {line}, column {column}: {message}"),
        None => message,
    }
}

/// The 1-based `(line, column)` of a byte offset in `text` — the content-free half of a parse
/// error's location, recomputed here because [`redacted_parse_message`] throws away the input the
/// parser would otherwise have used to render it.
///
/// Byte-indexed throughout so a multi-byte character before the offset cannot panic on a
/// non-boundary slice; the COLUMN is then counted in `chars`, matching what `toml` itself reports.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let bytes = text.as_bytes();
    let end = offset.min(bytes.len());
    let line_start = bytes[..end].iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let line = bytes[..line_start].iter().filter(|b| **b == b'\n').count() + 1;
    let column = std::str::from_utf8(&bytes[line_start..end])
        .map(|s| s.chars().count())
        .unwrap_or(end - line_start)
        + 1;
    (line, column)
}

/// Pull the offending key out of a `toml`/serde deserialize message.
///
/// Deliberately built on `Display` alone rather than `toml::de::Error`'s span/message accessors:
/// the backtick-quoted shape serde emits ("unknown field", "missing field") is serde's own and
/// stable across versions, while the parser's structured API is not something this crate should
/// pin itself to for one cosmetic field.
///
/// Returns `None` for a pure syntax error, which names a line and column but no key — the full
/// parser message is carried through in that case, so the file and location still reach the
/// operator.
pub(crate) fn key_from_parse_message(message: &str) -> Option<String> {
    const MARKERS: &[&str] = &["unknown field `", "missing field `", "for key `"];
    for &marker in MARKERS {
        let Some(at) = message.find(marker) else { continue };
        let rest = &message[at + marker.len()..];
        match rest.find('`') {
            Some(end) if end > 0 => return Some(rest[..end].to_string()),
            _ => continue,
        }
    }
    None
}

#[path = "error_tests.rs"]
#[cfg(test)]
mod error_tests;
