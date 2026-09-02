//! [`Config`] — DEPLOYMENT settings: where things live on this box. Full override chain.
//!
//! The distinguishing question is "would a second machine running the same strategy need a
//! different value?". Store roots, log directories and listen addresses all answer yes, which is
//! why they carry the full `defaults -> file -> env -> CLI` chain: a container overrides a path
//! with an env var, an operator overrides a port with a flag for one run, and neither is a risk
//! decision.
//!
//! Contrast [`crate::Preferences`], whose values would be the SAME on the second machine (they
//! express taste), and [`crate::Policy`], which stops at the file layer on purpose.
//!
//! ## Field provenance
//!
//! Each field replaces a variable that exists today, named in its doc comment. Two of them are
//! the reason the settings program was written at all:
//!
//! - **`VIKE_HIST_STORE` had FIVE rows** in `vike_ops::settings::SETTINGS` — vike-app,
//!   vike-backtest, vike-backfill, vike-datahub and vike-studio each read it with a *different*
//!   fallback chain. One field with one default is the fix; the divergent fallbacks become the
//!   callers' business, not the variable's. vike-app's row is the first to have MOVED here: its
//!   `studio_store_root` takes [`Config::store_root`] now, and passes it to the same
//!   `vike_model::store_path::resolve_store_root` precedence it always used.
//! - **`VIKE_LOG_DIR` and `VIKE_JOURNAL_DIR`** are each read from several crates, some as a
//!   direct `env::var` deep inside a library (`Layer::Library`, the registry's STEP-2 work list).
//!   Reading them HERE, from an injected map, is that work list's target state.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::layers::{get, CliOverride, CliOverrides, EnvOverride};

/// `VIKE_HIST_STORE` — the historical/tick store root.
pub const STORE_ROOT_ENV: &str = "VIKE_HIST_STORE";
/// `VIKE_LOG_DIR` — where the JSON daily-rolling log file is written.
pub const LOG_DIR_ENV: &str = "VIKE_LOG_DIR";
/// `VIKE_JOURNAL_DIR` — the live core's write-ahead command journal directory.
pub const JOURNAL_DIR_ENV: &str = "VIKE_JOURNAL_DIR";
/// `VIKE_STATE_DIR` — program-written strategy state.
pub const STATE_DIR_ENV: &str = "VIKE_STATE_DIR";
/// `VIKE_DATAHUB_ADDR` — the data-service listen/connect address.
pub const DATAHUB_ADDR_ENV: &str = "VIKE_DATAHUB_ADDR";
/// `VIKE_TRADEHUB_ADDR` — the headless trading daemon's control listen address.
pub const TRADEHUB_ADDR_ENV: &str = "VIKE_TRADEHUB_ADDR";
/// `VIKE_DATAHUB_ADVERTISE_ADDR` — the datahub dial address the trading daemon ADVERTISES.
pub const DATAHUB_ADVERTISE_ADDR_ENV: &str = "VIKE_DATAHUB_ADVERTISE_ADDR";

/// The `vike-datahub` default listen address, as its `main.rs` reads it today.
pub const DEFAULT_DATAHUB_ADDR: &str = "127.0.0.1:7878";

/// Deployment settings — paths and addresses for THIS machine.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    /// Root of the DataFusion/Parquet history + tick store. `None` = the caller's own fallback
    /// (today: five different ones — see the module doc).
    ///
    /// Was `VIKE_HIST_STORE`.
    pub store_root: Option<PathBuf>,

    /// Directory for the JSON daily-rolling log file. `None` = `<exe_dir>/logs`, `vike-log`'s
    /// own fallback.
    ///
    /// Was `VIKE_LOG_DIR`.
    pub log_dir: Option<PathBuf>,

    /// Directory for the live core's mmap write-ahead command journal.
    ///
    /// Was `VIKE_JOURNAL_DIR`.
    pub journal_dir: Option<PathBuf>,

    /// Directory for program-written strategy state. `None` resolves the project rung —
    /// `<project>/settings/state/strategy-state` — and falls back to `<exe_dir>/strategy-state`
    /// only when no project sits above the working directory. `crates/vike-app/src/main.rs`'s
    /// `state_dir_path` is the resolver, and it takes that project rung off the boot's own walk
    /// rather than re-walking. ⚠ This line named the `<exe_dir>` path as the flat default long
    /// after that stopped being true.
    ///
    /// Was `VIKE_STATE_DIR`.
    pub state_dir: Option<PathBuf>,

    /// CLIENT-side DIAL address of a `vike-datahub` data server (`host:port`). **Set → vike-app's
    /// Studio takes its history from that server over RPC** (a DataFusion-free
    /// `RemoteHistStore`; the Studio's run Backend also defaults to Remote at the same address);
    /// **unset — the default — → the Studio opens the LOCAL store** at `config.store_root`'s
    /// resolution, exactly as before the split-plane program (2026-08-18). Presence IS the
    /// branch, which is why this field lost its old always-set default.
    ///
    /// ⚠ Deliberately DISTINCT from [`Config::tradehub_addr`]: that one is the DAEMON'S BIND
    /// address (where `vike-tradehub` LISTENS on this machine); this one is where a CLIENT
    /// DIALS a datahub server, usually on another machine. The `vike-datahub` server bin does
    /// not read settings at all — its own listen address is its `VIKE_DATAHUB_ADDR` environment
    /// read, defaulting to [`DEFAULT_DATAHUB_ADDR`].
    ///
    /// Was `VIKE_DATAHUB_ADDR` (the variable still overrides the file layer here).
    pub datahub_addr: Option<String>,

    /// Address the headless `vike-tradehub` control surface listens on — the DAEMON'S BIND
    /// address, deliberately distinct from [`Config::datahub_addr`], which is a CLIENT'S dial
    /// address. `None` = no control server, which is also the safe default — that surface is
    /// opt-in.
    ///
    /// Was `VIKE_TRADEHUB_ADDR`.
    pub tradehub_addr: Option<String>,

    /// The datahub dial address the `vike-tradehub` daemon ADVERTISES to its clients
    /// (split-plane REQ-2): set → every `Welcome.features` the daemon's node server sends
    /// carries `datahub=<this value>`, and a connected client that has no explicit
    /// `datahub_addr` of its own adopts it — one configured address (the daemon's) reaches both
    /// planes. `None` — the default — advertises nothing, byte-identical to the pre-REQ-2
    /// Welcome. Advertisement, NEVER proxying: history/backtest traffic still dials the datahub
    /// directly; nothing routes through the process holding live orders.
    ///
    /// ⚠ THREE addresses, three jobs — this is the third:
    /// - [`Config::tradehub_addr`]: where the DAEMON LISTENS (its bind address, on the daemon's
    ///   box).
    /// - [`Config::datahub_addr`]: where THIS machine's CLIENT DIALS a datahub — an explicit
    ///   client-side setting that always WINS over any advertisement.
    /// - `datahub_advertise_addr` (this key, read on the DAEMON's box): where the daemon TELLS
    ///   clients to dial the datahub it fronts. It must be an address valid FROM THE CLIENT'S
    ///   side of the wire — for the tunnel-only posture both servers ship with, that is the
    ///   client-local tunnel mouth (e.g. `127.0.0.1:7878`, with `ssh -L` forwarding both ports),
    ///   not the daemon's private interface.
    ///
    /// Never read by the datahub server itself (its listen address stays its own
    /// `VIKE_DATAHUB_ADDR` read); consumed by `vike-tradehub`'s `start_observe_server`.
    pub datahub_advertise_addr: Option<String>,
}

impl Default for Config {
    /// Every field's absence means "the caller's own fallback applies" — including
    /// `datahub_addr`, whose absence is itself the answer ("no datahub — local store") now that
    /// vike-app's Studio branches on its presence.
    fn default() -> Self {
        Config {
            store_root: None,
            log_dir: None,
            journal_dir: None,
            state_dir: None,
            datahub_addr: None,
            tradehub_addr: None,
            datahub_advertise_addr: None,
        }
    }
}

/// The FILE shape of [`Config`] — all-optional, unknown keys rejected by name. See
/// [`crate::policy::PolicyPatch`] for why the loader patches instead of deserializing the
/// effective struct.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigPatch {
    /// See [`Config::store_root`].
    pub store_root: Option<PathBuf>,
    /// See [`Config::log_dir`].
    pub log_dir: Option<PathBuf>,
    /// See [`Config::journal_dir`].
    pub journal_dir: Option<PathBuf>,
    /// See [`Config::state_dir`].
    pub state_dir: Option<PathBuf>,
    /// See [`Config::datahub_addr`].
    pub datahub_addr: Option<String>,
    /// See [`Config::tradehub_addr`].
    pub tradehub_addr: Option<String>,
    /// See [`Config::datahub_advertise_addr`].
    pub datahub_advertise_addr: Option<String>,
}

impl Config {
    /// Fold one file's patch in. Paths are not checked for existence — a store root may legally
    /// be created on first write, and a loader that stats the filesystem stops being a pure
    /// function of its inputs.
    pub(crate) fn apply(&mut self, patch: ConfigPatch, file: &Path) -> Result<(), ConfigError> {
        if let Some(v) = patch.store_root {
            self.store_root = Some(v);
        }
        if let Some(v) = patch.log_dir {
            self.log_dir = Some(v);
        }
        if let Some(v) = patch.journal_dir {
            self.journal_dir = Some(v);
        }
        if let Some(v) = patch.state_dir {
            self.state_dir = Some(v);
        }
        if let Some(v) = patch.datahub_addr {
            check_addr(file, "datahub_addr", &v)?;
            self.datahub_addr = Some(v);
        }
        if let Some(v) = patch.tradehub_addr {
            check_addr(file, "tradehub_addr", &v)?;
            self.tradehub_addr = Some(v);
        }
        if let Some(v) = patch.datahub_advertise_addr {
            check_addr(file, "datahub_advertise_addr", &v)?;
            self.datahub_advertise_addr = Some(v);
        }
        Ok(())
    }
}

/// The shared `host:port` rule every override layer enforces: non-blank, and carrying a `:`
/// separator. Deliberately NOT a full `SocketAddr` parse: `vike-datahub` is reached through an
/// SSH tunnel and a hostname is a legitimate value, so resolving here would reject a working
/// config.
///
/// This is the ONE place the rule is spelled. `check_addr` (file layer), `check_env_addr` (env
/// layer) and `apply_cli`'s CLI-layer check (below) each wrap this in their own [`ConfigError`]
/// shape — a different variant with different fields per layer, which is why they cannot simply
/// call one another — but the boolean itself lives here exactly once, so a validation gap (a
/// missing blank check, a dropped `!`) can no longer exist in one copy while the others stay
/// fixed.
fn is_host_port(v: &str) -> bool {
    !v.trim().is_empty() && v.contains(':')
}

/// An address must at least be non-blank and carry a `host:port` separator. See [`is_host_port`]
/// for the shared rule.
fn check_addr(file: &Path, key: &str, v: &str) -> Result<(), ConfigError> {
    if is_host_port(v) {
        return Ok(());
    }
    Err(ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: format!("{v:?} is not a host:port address"),
    })
}

/// The env-layer twin of [`check_addr`] — same rule (see [`is_host_port`]), but the message
/// points at the shell.
fn check_env_addr(var: &str, v: &str) -> Result<(), ConfigError> {
    if is_host_port(v) {
        return Ok(());
    }
    Err(ConfigError::Env {
        var: var.to_string(),
        value: v.to_string(),
        message: "expected a host:port address".to_string(),
    })
}

impl EnvOverride for Config {
    fn apply_env(&mut self, env: &HashMap<String, String>) -> Result<(), ConfigError> {
        if let Some(v) = get(env, STORE_ROOT_ENV) {
            self.store_root = Some(PathBuf::from(v));
        }
        if let Some(v) = get(env, LOG_DIR_ENV) {
            self.log_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = get(env, JOURNAL_DIR_ENV) {
            self.journal_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = get(env, STATE_DIR_ENV) {
            self.state_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = get(env, DATAHUB_ADDR_ENV) {
            check_env_addr(DATAHUB_ADDR_ENV, v)?;
            self.datahub_addr = Some(v.to_string());
        }
        if let Some(v) = get(env, TRADEHUB_ADDR_ENV) {
            check_env_addr(TRADEHUB_ADDR_ENV, v)?;
            self.tradehub_addr = Some(v.to_string());
        }
        if let Some(v) = get(env, DATAHUB_ADVERTISE_ADDR_ENV) {
            check_env_addr(DATAHUB_ADVERTISE_ADDR_ENV, v)?;
            self.datahub_advertise_addr = Some(v.to_string());
        }
        Ok(())
    }
}

impl CliOverride for Config {
    fn apply_cli(&mut self, cli: &CliOverrides) -> Result<(), ConfigError> {
        if let Some(v) = &cli.store_root {
            self.store_root = Some(PathBuf::from(v));
        }
        if let Some(v) = &cli.log_dir {
            self.log_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = &cli.datahub_addr {
            // Same rule as `check_addr`/`check_env_addr` — see `is_host_port` — wrapped in the
            // CLI layer's own error shape (a flag name, not a file/key pair).
            if !is_host_port(v) {
                return Err(ConfigError::Cli {
                    flag: "addr".to_string(),
                    value: v.clone(),
                    message: "expected a host:port address".to_string(),
                });
            }
            self.datahub_addr = Some(v.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> &'static Path {
        Path::new("config.toml")
    }

    #[test]
    fn env_overrides_the_file_layer() {
        let mut c = Config::default();
        c.apply(
            ConfigPatch { store_root: Some("/from/file".into()), ..Default::default() },
            file(),
        )
        .unwrap();
        assert_eq!(c.store_root, Some(PathBuf::from("/from/file")));

        let env = HashMap::from([(STORE_ROOT_ENV.to_string(), "/from/env".to_string())]);
        c.apply_env(&env).unwrap();
        assert_eq!(c.store_root, Some(PathBuf::from("/from/env")));
    }

    #[test]
    fn cli_outranks_env() {
        let mut c = Config::default();
        c.apply_env(&HashMap::from([(STORE_ROOT_ENV.to_string(), "/from/env".to_string())]))
            .unwrap();
        c.apply_cli(&CliOverrides { store_root: Some("/from/cli".into()), ..Default::default() })
            .unwrap();
        assert_eq!(c.store_root, Some(PathBuf::from("/from/cli")));
    }

    #[test]
    fn a_malformed_address_is_rejected_naming_the_variable() {
        let env = HashMap::from([(DATAHUB_ADDR_ENV.to_string(), "localhost".to_string())]);
        let err = Config::default().apply_env(&env).unwrap_err();
        assert!(err.to_string().starts_with("VIKE_DATAHUB_ADDR=localhost: "), "{err}");
    }

    #[test]
    fn an_unset_variable_leaves_the_file_value_alone() {
        let mut c = Config::default();
        c.apply(ConfigPatch { log_dir: Some("/logs".into()), ..Default::default() }, file())
            .unwrap();
        c.apply_env(&HashMap::new()).unwrap();
        assert_eq!(c.log_dir, Some(PathBuf::from("/logs")));
        assert_eq!(c.datahub_addr, None, "unset stays unset — presence is the Studio's branch");
    }

    /// `check_addr` is the FILE-layer twin of `check_env_addr`
    /// (`a_malformed_address_is_rejected_naming_the_variable` above only exercises the ENV
    /// layer) — nothing else calls it, so it needs its own direct coverage through `Config::apply`.
    /// A value with no `:` must be rejected, naming the key.
    #[test]
    fn a_malformed_file_address_is_rejected() {
        let err = Config::default()
            .apply(
                ConfigPatch { datahub_addr: Some("localhost".to_string()), ..Default::default() },
                file(),
            )
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("datahub_addr"), "{msg}");
        assert!(msg.contains("localhost"), "{msg}");
    }

    /// The REQ-2 advertisement key rides `tradehub_addr`'s exact wiring: file layer with the
    /// shared `check_addr` rule, env layer (`VIKE_DATAHUB_ADVERTISE_ADDR`) overriding it, and a
    /// malformed value rejected at either layer naming the key/variable.
    #[test]
    fn datahub_advertise_addr_takes_the_file_then_env_chain() {
        let mut c = Config::default();
        c.apply(
            ConfigPatch {
                datahub_advertise_addr: Some("127.0.0.1:7878".to_string()),
                ..Default::default()
            },
            file(),
        )
        .unwrap();
        assert_eq!(c.datahub_advertise_addr, Some("127.0.0.1:7878".to_string()));

        let env =
            HashMap::from([(DATAHUB_ADVERTISE_ADDR_ENV.to_string(), "tunnel:9999".to_string())]);
        c.apply_env(&env).unwrap();
        assert_eq!(c.datahub_advertise_addr, Some("tunnel:9999".to_string()));

        let bad = HashMap::from([(DATAHUB_ADVERTISE_ADDR_ENV.to_string(), "noport".to_string())]);
        let err = Config::default().apply_env(&bad).unwrap_err();
        assert!(err.to_string().starts_with("VIKE_DATAHUB_ADVERTISE_ADDR=noport: "), "{err}");

        let err = Config::default()
            .apply(
                ConfigPatch {
                    datahub_advertise_addr: Some("noport".to_string()),
                    ..Default::default()
                },
                file(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("datahub_advertise_addr"), "{err}");
        assert_eq!(
            Config::default().datahub_advertise_addr,
            None,
            "unset advertises nothing — the pre-REQ-2 Welcome"
        );
    }

    /// A blank value is rejected too — "must at least be non-blank" is a separate half of the
    /// rule from "must contain a `:`", and both halves need to actually fire.
    #[test]
    fn a_blank_file_address_is_rejected() {
        let err = Config::default()
            .apply(
                ConfigPatch { tradehub_addr: Some("   ".to_string()), ..Default::default() },
                file(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("tradehub_addr"), "{err}");
    }

    /// A well-formed `host:port` value passes through unchanged — `check_addr` must not reject
    /// what it is supposed to accept.
    #[test]
    fn a_well_formed_file_address_is_accepted() {
        let mut c = Config::default();
        c.apply(
            ConfigPatch {
                datahub_addr: Some("127.0.0.1:9000".to_string()),
                tradehub_addr: Some("0.0.0.0:9100".to_string()),
                ..Default::default()
            },
            file(),
        )
        .unwrap();
        assert_eq!(c.datahub_addr, Some("127.0.0.1:9000".to_string()));
        assert_eq!(c.tradehub_addr, Some("0.0.0.0:9100".to_string()));
    }

    /// The predicate every layer's check wraps — pinned directly so a mutant flipping `!`,
    /// dropping the blank half, or dropping the colon half is caught here even before it reaches
    /// any one layer's error shape.
    #[test]
    fn is_host_port_accepts_only_non_blank_colon_bearing_values() {
        assert!(is_host_port("127.0.0.1:9000"));
        assert!(is_host_port("localhost:80"));
        // A colon is non-whitespace, so padding around it still counts as non-blank content.
        assert!(is_host_port("   :   "));
        assert!(!is_host_port("localhost"));
        assert!(!is_host_port(""));
        assert!(!is_host_port("   "));
    }

    /// `apply_cli`'s inline `datahub_addr` check used to duplicate `check_addr`'s rule rather
    /// than share it, and carried no direct test — two mutants survived there (the whole check
    /// replaced with `Ok(())`, and the `!` deleted) because nothing ever drove a bad CLI address
    /// through `apply_cli`. Now it shares `is_host_port` with the file/env layers, and this test
    /// exercises it through the real entry point: a malformed value is rejected, naming the flag.
    #[test]
    fn a_malformed_cli_address_is_rejected() {
        let err = Config::default()
            .apply_cli(&CliOverrides {
                datahub_addr: Some("localhost".to_string()),
                ..Default::default()
            })
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("addr"), "{msg}");
        assert!(msg.contains("localhost"), "{msg}");
    }

    /// The blank half of the rule, through the CLI layer.
    #[test]
    fn a_blank_cli_address_is_rejected() {
        let err = Config::default()
            .apply_cli(&CliOverrides {
                datahub_addr: Some("   ".to_string()),
                ..Default::default()
            })
            .unwrap_err();
        assert!(err.to_string().contains("addr"), "{err}");
    }

    /// A well-formed value passes through `apply_cli` unchanged.
    #[test]
    fn a_well_formed_cli_address_is_accepted() {
        let mut c = Config::default();
        c.apply_cli(&CliOverrides {
            datahub_addr: Some("127.0.0.1:9000".to_string()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(c.datahub_addr, Some("127.0.0.1:9000".to_string()));
    }
}
