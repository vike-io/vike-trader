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
//! CLI failures name the FLAG instead of a file, for the same reason — there is no file to open,
//! so the message points at the command line.

use std::fmt;
use std::path::PathBuf;

/// A settings load failure. Every variant names either a (file, key) pair or the CLI flag that
/// carried the bad value.
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

/// **What a refusal of a RETIRED key says about getting rid of the stale row** — the one sentence
/// every tombstone arm shares (`Policy::apply`'s `max_total_exposure` and `rate.max_utilization`,
/// `Preferences::apply`'s `rate_utilization`, `Config::apply`'s `state_dir`, the retired flag
/// keys), so they cannot drift. `dotted_key` is the key as `config set` spells it
/// (`policy.max_total_exposure`).
///
/// The verb is `vike-cli config unset` (decision 0161). It removes the one row and moves the
/// seal's row count down in the same transaction, so a store that was refusing to load because of
/// that row loads again.
///
/// ⚠ **A hand `DELETE` is the trap this sentence exists to keep an operator out of.** It changes
/// the `setting` row count that `settings_adoption.setting_rows` sealed, and [`crate::mirror`]'s
/// `adoption_integrity` then refuses the next boot as rows that "left by some other route" — the
/// only repair of THAT is restoring the settings database from the box's nightly backup, and the
/// backup still holds the stale row.
///
/// ⚠ It is worded so it is also true in a WRITE context: a refused `config set` of a retired key
/// reaches the same arm, where no row exists yet. "A stored row under this key is removed with …"
/// is a statement about the verb, not an instruction that presumes a row.
pub(crate) fn stale_row_removal(dotted_key: &str) -> String {
    format!(
        "A stored row under this key is removed with `vike-cli config unset {dotted_key}` (the \
         seal's row count moves with it in one transaction; a hand DELETE would trip the seal \
         instead), and a store that carries one cannot load until it is gone"
    )
}

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

    /// The refusal for a whole-number knob outside its CLOSED range `min..=max`: it names the
    /// key (through the `Display` shape), the value, BOTH bounds and what the range protects.
    ///
    /// One constructor for every bounded integer row, so no two of them word a refusal differently
    /// and none can forget a bound. Nothing clamps: a value outside is refused at the write and
    /// at the load, because a clamp is a setting the operator believes they filed and has not.
    pub(crate) fn out_of_range(
        file: &std::path::Path,
        key: &str,
        value: u32,
        min: u32,
        max: u32,
        why: &str,
    ) -> Self {
        ConfigError::Value {
            file: file.to_path_buf(),
            key: key.to_string(),
            message: format!("{value} is outside the allowed range {min}..={max} — {why}"),
        }
    }
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
