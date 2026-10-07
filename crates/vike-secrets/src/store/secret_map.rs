//! The credential map and its companions: `SecretMap`, `SecretsError` and `Source`.

use super::*;

/// A credential map. Redacts in `Debug`; never implements `Display`.
///
/// `Debug` shows the KEY NAMES and the count but never a value — the same balance
/// `vike_bridge_core::credentials::Credentials` strikes with `api_key=***{last4}`. Key names are
/// already public knowledge (they are the `vike_ops::settings` registry's whole subject matter);
/// values are the credential.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretMap(BTreeMap<String, String>);

impl SecretMap {
    pub fn new(map: BTreeMap<String, String>) -> Self {
        SecretMap(map)
    }

    /// Build from the `HashMap<String, String>` shape the rest of the workspace speaks.
    pub fn from_map(map: HashMap<String, String>) -> Self {
        SecretMap(map.into_iter().collect())
    }

    /// Hand back the `HashMap<String, String>` every existing credential call site expects. Named
    /// so the call site reads as the deliberate end of redaction that it is.
    pub fn into_map(self) -> HashMap<String, String> {
        self.0.into_iter().collect()
    }

    /// The sorted key names — safe to print, and what `vike-cli secrets list` shows.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for SecretMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretMap({} entries: [", self.0.len())?;
        for (i, k) in self.0.keys().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{k}=***")?;
        }
        f.write_str("])")
    }
}

/// The one thing that can go wrong: the store EXISTS and could not be read.
///
/// An ABSENT store is not this — it is [`Source::None`] and an empty map, which is the live gate
/// (no credentials ⇒ every venue stays paper). "Not configured" and "cannot open" must never look
/// the same to an operator, which is the whole reason this type exists.
///
/// Carries the PATH and the OS reason, never file contents — so `Debug`/`Display` are safe to log
/// verbatim, which is the point: an operator has to be able to see "the store did not open" in a
/// daemon log without that log becoming a credential.
#[derive(Debug)]
pub struct SecretsError {
    /// The file that could not be read.
    pub path: PathBuf,
    /// Why the OS refused.
    pub source: std::io::Error,
}

impl std::fmt::Display for SecretsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "credential store {} could not be read: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for SecretsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Where a resolved credential map came from — the provenance `vike-cli secrets` prints so an
/// operator is never guessing which file is live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Parsed from this file.
    File(PathBuf),
    /// Read from a TABLE in the settings database at this path —
    /// `docs/decisions/0054-settings-move-into-one-database.md`.
    ///
    /// ⚠ **A fourth word in a consumer-visible domain.** `vike-cli secrets path --json` prints this
    /// provenance, so a consumer that matched on the two old spellings sees a new one the day a box
    /// migrates. It is added rather than folded into [`Source::File`] because folding would print a
    /// `.db` path in a sentence that says "file" and give an operator a path they can `cat`, which
    /// is precisely the thing constraint 2 of 0054 says must be replaced before it is removed.
    Database(PathBuf),
    /// Neither store exists. An empty map: the live gate (no credentials ⇒ stay paper).
    None,
}
