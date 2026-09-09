//! API-key permission introspection seam + the live-arm withdraw policy (STEP-1 of the
//! api-key-permissions capability map).
//!
//! MOTIVATION (safety-default-ON, non-custodial product promise): a compromised terminal must be
//! structurally unable to move funds. So before arming a LIVE venue we want to know what the
//! configured API key is actually allowed to do — and REFUSE to arm one that can WITHDRAW.
//!
//! This module owns three things and NOTHING that reaches the wire:
//!   1. [`KeyPermissions`] — the venue-neutral answer (each field `Option<bool>`: `None` = Unknown,
//!      the value a venue with no such endpoint reports).
//!   2. [`KeyPermissionProbe`] — the seam a venue adapter implements to fetch its key permissions
//!      (the first concrete row is `vike_binance`'s `/sapi/v1/account/apiRestrictions` reader).
//!   3. The policy: [`withdraw_gate`] (pure) + [`allow_withdraw_keys`] (the `VIKE_ALLOW_WITHDRAW_KEYS`
//!      override, EXACT string `"1"`, same unfuzzy idiom as `VIKE_RECONCILE=1`). A KNOWN
//!      withdraw-capable key is [`WithdrawGate::Refuse`]d (the caller degrades to paper — the SAME
//!      outcome the absent-credentials gate produces); Unknown never refuses.
//!
//! This module ships the seam + one venue + the policy. ⚠ **STEP-2 IS DONE for binance, and the
//! paragraph that used to stand here said the opposite** — it read *"deliberately NOT wired into
//! `vike_mount::make_engine`'s live path … nothing calls [`withdraw_gate`] by default, so the
//! change is byte-identical when OFF"*, which stopped being true when the wiring landed and was
//! never revised. `crates/vike-mount/src/arming.rs`'s `binance_withdraw_gate` is called from
//! `vike_mount::make_engine`, resolved BEFORE the client match so its one blocking signed read is
//! visible at the top level; a `Refuse` makes the `("binance", Some(c))` arm not match and the
//! venue falls through to the paper arm, exactly as absent credentials do. A default build DOES
//! reach this code. Corrected 2026-09-01.
//!
//! What is still true: every OTHER `(venue, creds)` pair short-circuits to `Allow` with no network
//! and no behaviour change, so binance is the only venue this gate can refuse today.
//!
//! ⚠ **"Only binance" is close to the ceiling, not a backlog — do not read it as twelve venues
//! owed.** A survey of all fourteen roster venues against their current official API docs
//! (2026-09-01) found that only THREE more can be probed at all: bybit
//! (`GET /v5/user/query-api` → `permissions.Wallet` contains `"Withdraw"`), okx
//! (`GET /api/v5/account/config` → `perm`, a CSV token — split it, never substring-match) and
//! deribit (`public/auth` → `scope` contains `wallet:read_write`, which rides an auth call the
//! bridge already makes). The rest cannot be probed, for two DIFFERENT reasons that must not be
//! collapsed:
//!
//! - **No permission record exists and the trading credential cannot move funds at all** — oanda,
//!   ig, dukascopy, ctrader, fxcm. The credential is a login token; withdrawals are a portal
//!   operation to a pre-registered bank account. ibkr is a variant: withdrawal exists only on a
//!   different product, behind separately registered key material a CP-Gateway session cannot
//!   reach. For these, "no endpoint" is a SETTLED answer, not a documentation gap.
//! - **Withdrawal capability belongs to the KEY ITSELF and no server can report or revoke it** —
//!   polymarket, hyperliquid (master), aster. These hold an EOA private key that signs a plain
//!   ERC20 transfer with no venue involvement; this workspace's own
//!   `crates/bridges/polymarket/src/settlement/` cluster already signs on-chain `redeem_positions`
//!   with it, and it must, because settlement needs exactly that power. ⚠ So [`KeyPermissions`]'
//!   all-`None` UNKNOWN — which never refuses — is the WORST answer precisely where exposure is
//!   unbounded. A probe seam cannot fix this; the operator control is a dedicated wallet holding a
//!   working minimum.
//!
//! ⚠ **The `canWithdraw` trap.** aster's `/api/v3/account` returns a `canWithdraw` field and it
//! looks like a free probe. It must NOT be used as one without a live A/B: binance documents the
//! same field with no description at all, and the fact that binance needed a SEPARATE
//! `apiRestrictions` endpoint is strong evidence these are ACCOUNT-status flags rather than KEY
//! permissions. Confusing those two is the most dangerous error available here.
//!
//! Per the capability-map playbook a VENUES-iterating table ("which venues support introspection")
//! would be the way to record all of the above as data. It is still absent, so no completeness gate
//! is half-added — and the survey above is the input whenever somebody writes one.

use std::collections::HashMap;

/// What a venue's configured API key is allowed to do, normalized across venues. Each field is
/// `Option<bool>`: `Some(true)`/`Some(false)` when the venue's introspection endpoint reports it,
/// `None` when it is UNKNOWN — either the venue has no such endpoint, or the field was absent from
/// an otherwise-valid response. Contains no secrets, so `Debug` is safe to derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyPermissions {
    /// Whether the key can move funds off the account (the field the withdraw gate keys off).
    pub can_withdraw: Option<bool>,
    /// Whether the key can place/cancel orders (informational; a trade-only key is exactly what we
    /// WANT for a non-custodial terminal).
    pub can_trade: Option<bool>,
    /// Whether the venue restricts the key to an IP allowlist (informational; an extra defense a
    /// future policy could reward).
    pub ip_restricted: Option<bool>,
}

impl KeyPermissions {
    /// All-Unknown — the value a venue with NO introspection endpoint reports (and the value the
    /// policy treats as "do not refuse": fail-open on introspection, fail-safe only on a KNOWN
    /// withdraw capability). Equal to [`KeyPermissions::default`].
    pub const UNKNOWN: KeyPermissions =
        KeyPermissions { can_withdraw: None, can_trade: None, ip_restricted: None };
}

/// The introspection seam: a venue adapter implements this to report what its configured API key is
/// allowed to do, so the live-arm [`withdraw_gate`] can refuse a withdraw-capable key. A venue with
/// no such endpoint returns [`KeyPermissions::UNKNOWN`] (all-`None`), never an `Err` — an `Err` is
/// reserved for a genuine fetch/parse failure the caller decides how to treat (the reported step-2
/// wiring treats a fetch error as best-effort/Unknown, mirroring the permissive
/// `RiskLimits::from_properties` pre-fetch fallback).
pub trait KeyPermissionProbe {
    /// Fetch the live key permissions for this probe's configured credentials.
    fn fetch_key_permissions(&self) -> Result<KeyPermissions, String>;
}

/// The env var that permits arming a live venue whose key can withdraw. Read with the EXACT-`"1"`
/// idiom (see [`allow_withdraw_keys`]).
pub const ALLOW_WITHDRAW_KEYS_ENV: &str = "VIKE_ALLOW_WITHDRAW_KEYS";

/// The live-arm verdict for a fetched (or Unknown) key-permission set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithdrawGate {
    /// Safe to arm live — the key is known NOT to be withdraw-capable, OR its withdraw capability is
    /// Unknown (no endpoint / absent field), OR the operator override is set.
    Allow,
    /// Refuse to arm live — the key is KNOWN to be withdraw-capable and no override is set. The
    /// caller degrades this venue to paper: the SAME outcome the absent-credentials gate produces
    /// (`load_credentials_from` → `None` → stay paper).
    Refuse,
}

/// The pure withdraw policy: refuse ONLY a KNOWN withdraw-capable key (`can_withdraw == Some(true)`)
/// when `allow_override` is `false`. `Some(false)` (a trade-only key) and `None` (Unknown) both
/// [`Allow`](WithdrawGate::Allow); the override forces `Allow` regardless. Pure — the env read is
/// [`allow_withdraw_keys`], kept separate so this is testable without touching the process env.
pub fn withdraw_gate(perms: &KeyPermissions, allow_override: bool) -> WithdrawGate {
    let withdraw_capable = matches!(perms.can_withdraw, Some(true));
    if withdraw_capable && !allow_override { WithdrawGate::Refuse } else { WithdrawGate::Allow }
}

/// True iff `VIKE_ALLOW_WITHDRAW_KEYS` is the EXACT string `"1"` in `vars` — the same deliberately
/// unfuzzy idiom as `reconcile_enabled` (`"true"`/`"yes"`/`"0"` do NOT enable). The caller must pass
/// the REAL process env (`std::env::vars().collect()`), NOT the workspace `.env` credential map, so
/// a shell-exported override is visible — exactly as the `VIKE_RECONCILE` master gate is read.
pub fn allow_withdraw_keys(vars: &HashMap<String, String>) -> bool {
    vars.get(ALLOW_WITHDRAW_KEYS_ENV).map(String::as_str) == Some("1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A KNOWN withdraw-capable key is refused (no override).
    #[test]
    fn refuses_withdraw_enabled_key() {
        let perms = KeyPermissions { can_withdraw: Some(true), ..KeyPermissions::UNKNOWN };
        assert_eq!(withdraw_gate(&perms, false), WithdrawGate::Refuse);
    }

    /// A trade-only key (withdraw KNOWN-false) arms.
    #[test]
    fn allows_trade_only_key() {
        let perms = KeyPermissions {
            can_withdraw: Some(false),
            can_trade: Some(true),
            ip_restricted: Some(true),
        };
        assert_eq!(withdraw_gate(&perms, false), WithdrawGate::Allow);
    }

    /// The override bypasses the refusal for a withdraw-capable key.
    #[test]
    fn override_bypasses_refusal() {
        let perms = KeyPermissions { can_withdraw: Some(true), ..KeyPermissions::UNKNOWN };
        assert_eq!(withdraw_gate(&perms, true), WithdrawGate::Allow);
    }

    /// OFF/DEFAULT byte-identical invariant: a venue that reports Unknown (no endpoint / not yet a
    /// probe row) is NEVER refused with no override — so nothing changes for any venue until an
    /// adapter opts in AND its key is KNOWN withdraw-capable. `UNKNOWN` == `default()`.
    #[test]
    fn unknown_never_refuses_and_is_the_default() {
        assert_eq!(KeyPermissions::UNKNOWN, KeyPermissions::default());
        assert_eq!(withdraw_gate(&KeyPermissions::UNKNOWN, false), WithdrawGate::Allow);
        // Even a `None` withdraw with the other fields KNOWN stays Allow.
        let partial = KeyPermissions {
            can_withdraw: None,
            can_trade: Some(true),
            ip_restricted: Some(false),
        };
        assert_eq!(withdraw_gate(&partial, false), WithdrawGate::Allow);
    }

    /// The override reads the EXACT `"1"` (unfuzzy), off whatever map the caller passes.
    #[test]
    fn allow_withdraw_keys_true_only_for_exact_one() {
        assert!(allow_withdraw_keys(&map(&[("VIKE_ALLOW_WITHDRAW_KEYS", "1")])));
        assert!(!allow_withdraw_keys(&map(&[("VIKE_ALLOW_WITHDRAW_KEYS", "true")])));
        assert!(!allow_withdraw_keys(&map(&[("VIKE_ALLOW_WITHDRAW_KEYS", "0")])));
        assert!(!allow_withdraw_keys(&map(&[("VIKE_ALLOW_WITHDRAW_KEYS", "")])));
        assert!(!allow_withdraw_keys(&map(&[])));
    }
}
