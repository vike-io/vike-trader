//! Persisting a ROTATED cTrader OAuth grant back into the credential store — the write half of the
//! token loop, and the pure expiry arithmetic that decides when to rotate.
//!
//! # The loop this closes
//!
//! `crates/bridges/ctrader/CLAUDE.md` described a credential loop closed BY HAND: the authorize bin
//! wrote a token file nothing read back, `crate::config::CtraderConfig::from_vars` built the venue
//! config from the credential map only, and `crate::conn`'s `try_refresh_and_reauth` assigned a
//! refreshed pair IN MEMORY and returned. The refreshed credential therefore lived exactly as long
//! as the process, and — because cTrader ROTATES the refresh token on every refresh — the pair on
//! disk went stale the first time a daemon refreshed. An operator whose demo grant aged out got a
//! daemon that would not start while the credentials to start it sat in the same file. Observed
//! live: the alpaca+ctrader live rehearsal (PR #1407), Evidence 2.
//!
//! # ONE credential, ONE home: `<project>/settings/secrets.env`
//!
//! The grant is a credential, so it lives where every other credential lives. It is written through
//! [`vike_secrets::save_credentials`] — the byte-preserving UPSERT that replaces exactly the named
//! keys in place and leaves every comment, blank line, unrelated key and their ordering verbatim,
//! written back atomically. That is the ONE sanctioned way to write the store, and it is what the
//! workspace rule actually forbids: rewriting the user's only copy of their keys WHOLESALE, never
//! amending one key it was asked to amend.
//!
//! ⚠ The authorize bin's `<project>/settings/state/ctrader_token.json` is NOT a second source of
//! truth and is not read here. It stays that tool's own output record; the daemon reads the store
//! and writes the store.
//!
//! # Secrets never reach a log — or a ledger
//!
//! [`PersistError`] carries the store PATH, the KEY NAMES and an OS reason — never a token value.
//! The keys are names, not secrets; naming them is what makes a failure actionable ("could not write
//! CTRADER_DEMO_ACCESS_TOKEN to …").
//!
//! ⚠ This module used to log NOTHING AT ALL, which meant a venue-driven credential rotation was
//! invisible on every surface in the workspace. [`persist`] now appends one
//! `vike_model::change_journal` `credential_write` record per rotation, and [`record_rotation`]
//! emits exactly one `tracing::warn!` — when that APPEND fails. Both carry key NAMES and neither can
//! carry a value: `vike_model::change_journal::Change::credential_write` accepts no old/new/value
//! parameter, so a token has no way into the record even by mistake.

use std::path::{Path, PathBuf};

use vike_bridge_core::credentials::Environment;
use vike_model::account_keys::{AccountLabel, account_key};

/// The exact command that re-issues a cTrader grant. Named in every refusal a fresh grant would
/// fix, because "Access token expired" alone tells an operator what broke and not what to do — the
/// gap the live rehearsal hit, where a daemon refused startup and said nothing about the remedy.
pub const REAUTHORIZE_CMD: &str = "cargo run -p vike-ctrader --bin ctrader_authorize";

/// Refresh this long BEFORE the grant lapses, rather than after it has been refused.
///
/// ⚠ This is the knob that turns the loop from REACTIVE into PROACTIVE, and it is sized against
/// what actually goes wrong: a daemon that only refreshes on a rejection has already failed a
/// request. cTrader demo grants carry a multi-week `expiresIn`, so five minutes is negligible
/// against the token's life while being comfortably longer than the actor's 10 s heartbeat cadence
/// — the tick that evaluates it (`crate::conn`'s actor loop, never the fold).
pub const REFRESH_MARGIN_MS: i64 = 5 * 60 * 1000;

/// The store keys one tier's grant occupies — the SAME names
/// [`crate::config::CtraderConfig::from_vars`] reads, derived from one place so a rotation can
/// never write a key the loader does not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenKeys {
    pub access: String,
    pub refresh: String,
}

impl TokenKeys {
    /// `CTRADER_{DEMO|LIVE|SIM}_ACCESS_TOKEN` / `_REFRESH_TOKEN`.
    #[must_use]
    pub fn for_env(env: Environment) -> Self {
        Self::for_account(env, &AccountLabel::Default)
    }

    /// [`TokenKeys::for_env`] for ONE NAMED ACCOUNT —
    /// `CTRADER_{TIER}_ACCESS_TOKEN__{LABEL}` / `_REFRESH_TOKEN__{LABEL}`.
    ///
    /// ⚠ **A rotation WRITES these names**, which is why this exists rather than the loader alone
    /// being made account-aware. `CtraderConfig::from_vars_with_store_for_account` reads a labelled
    /// account's grant and must persist the refreshed one back to the SAME labelled keys: a
    /// [`TokenKeys::for_env`] here would let a second account's routine refresh overwrite the
    /// DEFAULT account's tokens in the shared `secrets.env`, breaking the account that was working.
    ///
    /// [`AccountLabel::Default`] is [`TokenKeys::for_env`], reached through it — the same two
    /// `String`s, since `vike_model::account_keys::account_key` returns its input unchanged there.
    #[must_use]
    pub fn for_account(env: Environment, label: &AccountLabel) -> Self {
        let tier = env.as_str();
        TokenKeys {
            access: account_key(&format!("CTRADER_{tier}_ACCESS_TOKEN"), label),
            refresh: account_key(&format!("CTRADER_{tier}_REFRESH_TOKEN"), label),
        }
    }
}

/// Everything a running connection needs in order to persist a rotated grant: WHERE the store is,
/// WHICH keys this tier occupies, and WHERE the rotation is recorded.
///
/// `None` on a `ConnConfig` (every caller that does not supply one — tests, the catalog probe)
/// keeps the historical behaviour exactly: a refresh lives as long as the process and nothing is
/// written — no store write, and therefore no journal record either. That is the honest shape: a
/// mount with no project resolved has no store to rotate INTO, so there is nothing to record and no
/// path to invent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenPersist {
    /// The credential store, as the COMPOSITION ROOT resolved it — a parameter, never a path this
    /// library walks for itself. See [`store_path_beside_state_dir`].
    pub store: PathBuf,
    pub keys: TokenKeys,
    /// `<project>/settings/state`, the SAME resolved directory [`TokenPersist::store`] was derived
    /// from — the change journal's home (`vike_model::change_journal::ChangeJournal::in_state_dir`
    /// joins `changes` onto it).
    ///
    /// ⚠ Not an `Option`, and not a second walk. Both fields are produced together by one
    /// `CtraderConfig::from_vars_with_store` call out of one composition-root parameter, so a
    /// rotation can never record itself into one project's ledger while writing another project's
    /// store. Where the root resolved no state directory there is no `TokenPersist` at all.
    pub state_dir: PathBuf,
}

/// The credential store that sits beside a project's `settings/state` directory:
/// `<project>/settings/state` → `<project>/settings/secrets.env`.
///
/// ⚠ A DERIVATION, not a second resolution walk. The root `CLAUDE.md`'s rule is that ONE walk
/// decides and every project-relative path is derived from it — the `_from`-less resolvers are
/// `$VIKE_SETTINGS_DIR`-blind, and all three shipped units set that variable, so a library that
/// re-walked from its working directory would answer for the wrong project on every deployment.
/// The caller passes the state directory its boot already resolved.
#[must_use]
pub fn store_path_beside_state_dir(state_dir: &Path) -> Option<PathBuf> {
    state_dir.parent().map(|settings| settings.join(vike_secrets::SECRETS_FILE))
}

/// Wall-clock ms at which a grant obtained at `obtained_at_ms` with lifetime `expires_in` seconds
/// lapses. `None` when the venue reported no lifetime — an unknown expiry must read as "no
/// opinion", never as a timestamp in the past that would trigger a refresh storm.
///
/// Saturating throughout, so an absurd `expires_in` reads as very far away rather than wrapping.
#[must_use]
pub fn expires_at_ms(obtained_at_ms: i64, expires_in: u64) -> Option<i64> {
    if expires_in == 0 {
        return None;
    }
    let life_ms = i64::try_from(expires_in).unwrap_or(i64::MAX / 2).saturating_mul(1000);
    Some(obtained_at_ms.saturating_add(life_ms))
}

/// Is a grant expiring at `expires_at_ms` due for a PROACTIVE refresh at `now_ms`?
///
/// `None` (an unknown expiry — a process that has not refreshed yet, so it never saw an
/// `expiresIn`) is deliberately NOT due: it must leave the reactive path exactly as it was rather
/// than refreshing on every heartbeat forever.
#[must_use]
pub fn refresh_due(expires_at_ms: Option<i64>, now_ms: i64) -> bool {
    match expires_at_ms {
        Some(at) => now_ms >= at.saturating_sub(REFRESH_MARGIN_MS),
        None => false,
    }
}

/// Why a rotated grant could not be persisted. Carries the store path, the key names and an OS
/// reason — NEVER a token value.
#[derive(Debug)]
pub struct PersistError {
    pub store: PathBuf,
    pub keys: TokenKeys,
    pub reason: String,
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "could not persist the refreshed cTrader grant ({} / {}) to {}: {} — the running \
             session keeps the new token, but a RESTART will fall back to the stale pair on disk",
            self.keys.access,
            self.keys.refresh,
            self.store.display(),
            self.reason
        )
    }
}

impl std::error::Error for PersistError {}

/// Upsert a freshly-refreshed grant onto its two store keys, preserving every other byte of the
/// file — and RECORD the rotation in the append-only change journal.
///
/// Both keys are written in ONE upsert: cTrader rotates the refresh token alongside the access
/// token, so persisting only the access token would leave the store carrying a refresh token that
/// has already been spent — the exact staleness this function exists to remove, in a subtler form.
/// **The record follows that shape rather than the call count: ONE rotation is ONE record carrying
/// both key names and `count == 2`.** Two records would read as two rotations, which is precisely
/// the half-write story an incident review must not be told.
///
/// # ⚠ This site was invisible EVERYWHERE before the journal
///
/// The other two `vike_secrets::save_credentials` callers at least emit a `tracing` line. This one
/// emitted nothing at all — the module doc's "nothing in this module logs" is still true, and still
/// deliberate — so a venue-driven credential rotation left no trace on any surface. `crate::conn`'s
/// `adopt_refreshed_token` does log the outcome, but that is one function away and is a console line
/// with a rolling-file retention measured in days. The ledger is the durable answer to *"when did
/// this grant last rotate"*.
///
/// # `now_ms` is a parameter, and the actor is the VENUE
///
/// `vike_model::change_journal` reads no clock, so the instant comes from the caller — which already
/// takes one (`crate::conn`'s `wall_clock_ms`, off the fold, at a heartbeat tick or after an OAuth
/// round trip). The record's actor is `Actor::venue("ctrader")` rather than any human channel,
/// because that is literally what happened: the venue rotated the grant and this process wrote down
/// what it was handed. That origin exists in the journal for this call site.
///
/// # A journal failure does not fail the rotation
///
/// The store write happens FIRST and its error is returned unchanged. A journal append that fails
/// afterwards is `warn!`ed with the DIRECTORY and the key names — never a token — and the call still
/// succeeds: the fresh grant IS on disk, and refusing to acknowledge it would turn a successful
/// self-heal into an outage over a missing audit line. (`warn` rather than the `error` its GUI twin
/// uses, because `crate::conn`'s caller already escalates a persist FAILURE and this is the weaker
/// half of a rotation that otherwise worked.)
///
/// ⚠ Nothing here can put a token in the ledger: `vike_model::change_journal::Change::credential_write`
/// takes no old/new/value parameter, and the only cells fed to it are the two KEY NAMES off
/// [`TokenKeys`], which are compile-time-shaped strings this module builds itself.
pub fn persist(
    persist: &TokenPersist,
    token: &crate::oauth::Token,
    now_ms: i64,
) -> Result<(), PersistError> {
    let updates = vec![
        (persist.keys.access.clone(), token.access_token.clone()),
        (persist.keys.refresh.clone(), token.refresh_token.clone()),
    ];
    vike_secrets::save_credentials(&persist.store, &updates).map_err(|e| PersistError {
        store: persist.store.clone(),
        keys: persist.keys.clone(),
        reason: e.to_string(),
    })?;

    record_rotation(persist, now_ms);
    Ok(())
}

/// The durable half of [`persist`]: ONE `credential_write` record for the pair that just rotated.
///
/// Split out so the record's construction is nameable and testable on its own, and so [`persist`]'s
/// store write reads as the one fallible step it is.
///
/// ⚠ It appends DIRECTLY rather than through `vike_connections::save_credentials_journalled`, the
/// wrapper the two GUI sites share. That crate is layer 75 and links `egui`; this bridge is layer 40
/// and is mounted by a headless daemon. The shared home is unreachable from here by construction,
/// not by preference.
fn record_rotation(persist: &TokenPersist, now_ms: i64) {
    use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome, Proc};

    // `Proc::current` reads `current_exe`, so resolve it once per process rather than per rotation.
    static PROCESS: std::sync::OnceLock<Proc> = std::sync::OnceLock::new();
    let process = PROCESS.get_or_init(|| Proc::current(env!("CARGO_PKG_VERSION")));

    let journal = ChangeJournal::in_state_dir(&persist.state_dir, process.clone());
    let store =
        persist.store.file_name().and_then(|n| n.to_str()).unwrap_or(vike_secrets::SECRETS_FILE);
    // ONE record, BOTH keys — the pair rotates together, so it is one change.
    let change = Change::credential_write(
        Outcome::Applied,
        Actor::venue("ctrader"),
        store,
        "ctrader",
        tier_of(&persist.keys),
        &[persist.keys.access.as_str(), persist.keys.refresh.as_str()],
    );
    if let Err(e) = journal.append(now_ms, &change) {
        tracing::warn!(
            target: "ctrader",
            error = %e,
            dir = %journal.dir().display(),
            access_key = %persist.keys.access,
            refresh_key = %persist.keys.refresh,
            "rotated cTrader grant NOT recorded to the change journal (the grant IS saved)"
        );
    }
}

/// The tier cell for a rotation's record, recovered by asking [`TokenKeys::for_env`] which tier
/// produces these keys — never carried, and never parsed back out of the string.
///
/// # Why not an `env: Environment` field on [`TokenPersist`]
///
/// It is the obvious alternative and it is the worse one: [`TokenKeys`] has public fields and can be
/// built directly, so a carried tier could DISAGREE with the keys actually written — and a ledger
/// line saying `DEMO` about a key that rotated the LIVE grant is exactly the failure this channel
/// exists to make impossible.
///
/// # Why not string surgery either
///
/// Splitting `CTRADER_{tier}_ACCESS_TOKEN` back apart would re-implement the naming
/// [`TokenKeys::for_env`] owns, in a second place, where the two could drift. Round-tripping through
/// the BUILDER cannot: if `for_env` changes shape, this changes with it for free. (It also keeps a
/// bare `CTRADER_`-shaped literal out of this file, which
/// `crates/vike-ops/tests/settings_registry.rs`'s literal harvest reads as an undeclared environment
/// variable — a real objection, and the cure for it happens to be the better code.)
///
/// A tier this loop does not cover yields the empty string rather than a guess: a `tier` claiming
/// `"LIVE"` about keys it could not identify is the one answer worse than none.
fn tier_of(keys: &TokenKeys) -> &'static str {
    [Environment::Sim, Environment::Demo, Environment::Live]
        .into_iter()
        .find(|env| TokenKeys::for_env(*env) == *keys)
        .map_or("", |env| env.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(access: &str, refresh: &str, expires_in: u64) -> crate::oauth::Token {
        crate::oauth::Token {
            access_token: access.to_string(),
            refresh_token: refresh.to_string(),
            expires_in,
        }
    }

    /// A DEMO-tier persist target under `dir`, with the state directory beside the store exactly as
    /// `CtraderConfig::from_vars_with_store` derives the pair.
    fn demo_persist(dir: &Path, store: PathBuf) -> TokenPersist {
        TokenPersist {
            store,
            keys: TokenKeys::for_env(Environment::Demo),
            state_dir: dir.join("state"),
        }
    }

    /// 2026-08-21T00:00:00Z — the injected instant every rotation below is stamped with.
    const T: i64 = 1_787_356_800_000;

    /// The key names are the ones the LOADER reads — pinned per tier, because a rotation that wrote
    /// a key `from_vars` does not read would silently persist nothing.
    #[test]
    fn the_persisted_keys_are_the_keys_the_loader_reads() {
        let demo = TokenKeys::for_env(Environment::Demo);
        assert_eq!(demo.access, "CTRADER_DEMO_ACCESS_TOKEN");
        assert_eq!(demo.refresh, "CTRADER_DEMO_REFRESH_TOKEN");
        let live = TokenKeys::for_env(Environment::Live);
        assert_eq!(live.access, "CTRADER_LIVE_ACCESS_TOKEN");
        assert_eq!(live.refresh, "CTRADER_LIVE_REFRESH_TOKEN");
    }

    /// The store sits beside the state directory the root declared — a derivation, never a walk.
    #[test]
    fn the_store_is_derived_from_the_declared_state_dir() {
        let state = Path::new("/srv/vike-<unit>/settings/state");
        assert_eq!(
            store_path_beside_state_dir(state),
            Some(PathBuf::from("/srv/vike-<unit>/settings").join(vike_secrets::SECRETS_FILE))
        );
    }

    /// THE property the whole change rests on: a refreshed pair lands on its two keys and EVERY
    /// other line of the store is byte-identical — comments, blank lines, other venues' keys and
    /// their order. This is the store-side twin of `vike_secrets::env_write`'s own suite, asserted
    /// here through the REAL file write so the atomic path is covered too.
    #[test]
    fn a_refresh_rewrites_only_its_two_keys_and_preserves_every_other_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("secrets.env");
        let original = "\
# vike credential store — hand-edited, keep the comments
BINANCE_LIVE_API_KEY=binance-key
BINANCE_LIVE_API_SECRET=binance-secret

# cTrader (the OAuth pair rotates on refresh)
CTRADER_CLIENT_ID=app-id
CTRADER_CLIENT_SECRET=app-secret
CTRADER_DEMO_ACCESS_TOKEN=AT_stale
CTRADER_DEMO_REFRESH_TOKEN=RT_stale
CTRADER_DEMO_ACCOUNT_ID=12345

OKX_DEMO_API_PASSPHRASE=okx-pass
";
        std::fs::write(&store, original).unwrap();

        let p = demo_persist(dir.path(), store.clone());
        persist(&p, &token("AT_fresh", "RT_fresh", 2_592_000), T).unwrap();

        let after = std::fs::read_to_string(&store).unwrap();
        let before_lines: Vec<&str> = original.lines().collect();
        let after_lines: Vec<&str> = after.lines().collect();
        assert_eq!(before_lines.len(), after_lines.len(), "no line may be added or dropped");

        for (i, (b, a)) in before_lines.iter().zip(after_lines.iter()).enumerate() {
            if b.starts_with("CTRADER_DEMO_ACCESS_TOKEN")
                || b.starts_with("CTRADER_DEMO_REFRESH_TOKEN")
            {
                continue;
            }
            assert_eq!(b, a, "line {i} must be byte-identical: {b:?} -> {a:?}");
        }
        assert_eq!(after_lines[7], "CTRADER_DEMO_ACCESS_TOKEN=AT_fresh");
        assert_eq!(after_lines[8], "CTRADER_DEMO_REFRESH_TOKEN=RT_fresh");
        assert!(!after.contains("AT_stale"), "the spent access token must not survive");
        assert!(!after.contains("RT_stale"), "the spent refresh token must not survive");
    }

    /// A persisted grant is picked up by a FRESH loader — the end-to-end proof that the write half
    /// and the read half agree, driven through the real `from_vars`.
    #[test]
    fn a_persisted_grant_is_picked_up_by_a_fresh_loader() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("secrets.env");
        std::fs::write(
            &store,
            "CTRADER_CLIENT_ID=app\nCTRADER_CLIENT_SECRET=sec\n\
             CTRADER_DEMO_ACCESS_TOKEN=AT_stale\nCTRADER_DEMO_REFRESH_TOKEN=RT_stale\n",
        )
        .unwrap();

        let p = demo_persist(dir.path(), store.clone());
        persist(&p, &token("AT_rotated", "RT_rotated", 100), T).unwrap();

        // A cold start: re-read the store exactly as a composition root would, then load the venue
        // config from it.
        let vars = vike_secrets::parse_dotenv(&std::fs::read_to_string(&store).unwrap());
        let cfg = crate::config::CtraderConfig::from_vars(Environment::Demo, &vars)
            .expect("the rotated grant gates live");
        assert_eq!(cfg.access_token, "AT_rotated");
        assert_eq!(cfg.refresh_token, "RT_rotated");
    }

    /// An unwritable store is an ERROR that names the path and the keys, and never the token.
    #[test]
    fn a_persist_failure_names_the_path_and_keys_but_never_the_token() {
        let dir = tempfile::tempdir().unwrap();
        // A DIRECTORY where the store should be: the write cannot succeed.
        let store = dir.path().join("secrets.env");
        std::fs::create_dir(&store).unwrap();

        let p = demo_persist(dir.path(), store.clone());
        let err = persist(&p, &token("AT_SECRET_LEAK", "RT_SECRET_LEAK", 10), T)
            .expect_err("a directory is not writable as a file");
        let msg = err.to_string();
        assert!(msg.contains(&store.display().to_string()), "{msg}");
        assert!(msg.contains("CTRADER_DEMO_ACCESS_TOKEN"), "{msg}");
        assert!(!msg.contains("AT_SECRET_LEAK"), "leaked the access token: {msg}");
        assert!(!msg.contains("RT_SECRET_LEAK"), "leaked the refresh token: {msg}");
    }

    /// The expiry arithmetic and the margin, exactly — driven by an injected `now_ms`, never a wall
    /// clock.
    #[test]
    fn refresh_is_due_only_inside_the_margin() {
        let at = expires_at_ms(1_000_000, 2_592_000).expect("a real lifetime has an expiry");
        assert_eq!(at, 1_000_000 + 2_592_000 * 1000);

        assert!(!refresh_due(Some(at), at - REFRESH_MARGIN_MS - 1));
        assert!(refresh_due(Some(at), at - REFRESH_MARGIN_MS));
        assert!(refresh_due(Some(at), at + 1), "an ALREADY-lapsed grant is due");
        // An unknown expiry is never due — it must not refresh on every heartbeat forever.
        assert!(!refresh_due(None, i64::MAX));
        // …and a venue that reported no lifetime yields no opinion rather than an instant refresh.
        assert_eq!(expires_at_ms(5, 0), None);
    }

    /// **A rotation on a LABELLED account must target that account's own keys.**
    ///
    /// This is the one way this venue could DAMAGE a credential file rather than merely mis-read
    /// one: [`persist`] upserts whatever [`TokenKeys`] names, so a `for_env` here would make every
    /// routine refresh on a second account overwrite the DEFAULT account's grant in the shared
    /// `secrets.env` — breaking the account that was working, silently, on a timer.
    ///
    /// The default account's two names are asserted as an EQUALITY against [`TokenKeys::for_env`]
    /// rather than as literals, so this is byte-identity rather than a second copy of the spelling.
    #[test]
    fn a_labelled_accounts_rotation_targets_its_own_keys_and_the_default_accounts_do_not_move() {
        let alt = AccountLabel::parse("ALT").expect("a legal label");
        for env in [Environment::Demo, Environment::Live, Environment::Sim] {
            let default = TokenKeys::for_account(env, &AccountLabel::Default);
            assert_eq!(
                default,
                TokenKeys::for_env(env),
                "the default account's keys must not move"
            );

            let labelled = TokenKeys::for_account(env, &alt);
            assert_eq!(labelled.access, format!("{}__ALT", default.access));
            assert_eq!(labelled.refresh, format!("{}__ALT", default.refresh));
            assert_ne!(
                labelled.access, default.access,
                "a labelled rotation must not write the default account's access token"
            );
            assert_ne!(labelled.refresh, default.refresh);
        }
    }
}
