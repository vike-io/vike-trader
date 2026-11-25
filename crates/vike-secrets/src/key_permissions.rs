//! API-key permission introspection seam + the live-arm withdraw policy.
//!
//! A compromised terminal must be structurally unable to move funds (the non-custodial promise),
//! so before arming a LIVE venue we want to know what the configured API key may do — and REFUSE
//! to arm one that can WITHDRAW.
//!
//! This module owns three things and NOTHING that reaches the wire:
//!   1. [`KeyPermissions`] — the venue-neutral answer (each field `Option<bool>`: `None` = Unknown,
//!      the value a venue with no such endpoint reports).
//!   2. [`KeyPermissionProbe`] — the seam a venue adapter implements to fetch its key permissions
//!      (`vike_binance`'s `/sapi/v1/account/apiRestrictions` reader; bybit, okx and hyperliquid hand the
//!      gate a closure over the pure readers below).
//!   3. The policy: [`withdraw_gate`] (pure), whose override is a `bool` — the resolved
//!      `flags.allow_withdraw_keys` row, handed to every start as
//!      `vike_bridge_core::venue_adapter::AdapterFlags::allow_withdraw_keys`; no credential-map key carries
//!      it, so no credential-store row can arm it. A KNOWN withdraw-capable key is
//!      [`CredentialWithdrawGate::Refuse`]d (the account does not run — the SAME outcome the
//!      absent-credentials gate produces); Unknown never refuses.
//!
//! Four venues are wired, each through [`withdraw_gate_before_start`] from its live start, before anything
//! is built: binance, bybit, okx and hyperliquid. The one blocking signed read happens before the exec client
//! exists, and on a `Refuse` the account is NOT RUNNING, with no paper client in its place (decision 0156).
//! A default build reaches this code.
//!
//! ⚠ **Four is the ceiling, not a backlog — do not read the rest as venues owed.** A survey of the
//! roster venues against their official API docs found that only these can be probed: bybit
//! (`GET /v5/user/query-api` → `permissions.Wallet` contains `"Withdraw"`), okx
//! (`GET /api/v5/account/config` → `perm`, a CSV token — split it, never substring-match),
//! hyperliquid (`userRole` of the signing address: the account's own key, or an approved agent wallet) and
//! deribit (`public/auth` → `scope` contains `wallet:read_write`, which rides an auth call the
//! bridge already makes). Deribit reports, but has no LIVE arm, so nothing of it is gated. The rest cannot
//! be probed, for DIFFERENT reasons that must not be collapsed:
//!
//! - **No permission record exists and the trading credential cannot move funds at all** — oanda,
//!   ig, dukascopy, ctrader, fxcm. The credential is a login token; withdrawals are a portal
//!   operation to a pre-registered bank account. ibkr is a variant: withdrawal exists only on a
//!   different product, behind separately registered key material a CP-Gateway session cannot
//!   reach. For these, "no endpoint" is a SETTLED answer, not a documentation gap.
//! - **No permission record exists and the key CAN move funds** — alpaca. Its bridge signs with
//!   Broker API keys, and the same key posts an account's `/v1/accounts/{id}/transfers`
//!   (`crates/bridges/alpaca/src/account.rs`'s `fund_test_account`); the Broker API reports no key
//!   permission, so there is nothing to probe and the operator control is the broker account's own.
//! - **Withdrawal capability belongs to the KEY ITSELF and no server can report or revoke it** —
//!   polymarket, hyperliquid (master), aster. These hold an EOA private key that signs a plain
//!   ERC20 transfer with no venue involvement (hyperliquid's `userRole` at least NAMES a master key as
//!   the signer, which is what its gate refuses); this workspace's own
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
//!
//! **The shared start gate.** [`withdraw_gate_before_start`] is the gate a live start runs (binance's former
//! own gate, with the venue, the account and the words as parameters), and the pure readers of the three
//! other answers it is fed are [`bybit_key_permissions`],
//! [`okx_key_permissions`] and [`hyperliquid_key_permissions`], each tested on the venue's real answer (the
//! captured DEMO bodies `crates/bridges/bybit/tests/fixtures/captured/query_api_demo.json` and
//! `crates/bridges/okx/tests/fixtures/captured/account_config_demo.json`). A permission is an EXACT word, never
//! a substring: a bybit `Wallet` element, an okx `perm` token.

use serde_json::Value;

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
/// reserved for a genuine fetch/parse failure the caller decides how to treat (binance's mount
/// treats it as Unknown: fail-open, with a warning).
pub trait KeyPermissionProbe {
    /// Fetch the live key permissions for this probe's configured credentials.
    fn fetch_key_permissions(&self) -> Result<KeyPermissions, String>;
}

/// The live-arm verdict for a fetched (or Unknown) key-permission set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialWithdrawGate {
    /// Safe to arm live — the key is known NOT to be withdraw-capable, OR its withdraw capability is
    /// Unknown (no endpoint / absent field), OR the operator override is set.
    Allow,
    /// Refuse to arm live — the key is KNOWN to be withdraw-capable and no override is set. The
    /// account does not run: the SAME outcome the absent-credentials gate produces
    /// (`api_credentials` → `None` → do not run).
    Refuse,
}

/// The pure withdraw policy: refuse ONLY a KNOWN withdraw-capable key (`can_withdraw == Some(true)`)
/// when `allow_override` is `false`. `Some(false)` (a trade-only key) and `None` (Unknown) both
/// [`Allow`](CredentialWithdrawGate::Allow); the override forces `Allow` regardless. Pure — the override
/// is the resolved `flags.allow_withdraw_keys` row, read by the caller.
pub fn withdraw_gate(perms: &KeyPermissions, allow_override: bool) -> CredentialWithdrawGate {
    let withdraw_capable = matches!(perms.can_withdraw, Some(true));
    if withdraw_capable && !allow_override {
        CredentialWithdrawGate::Refuse
    } else {
        CredentialWithdrawGate::Allow
    }
}

/// **May this live start's key arm?** The one gate every venue whose API reports key permissions runs before
/// anything is built.
///
/// - `mainnet == false` ⇒ `Allow` with NO network call: `fetch_permissions` is never called.
/// - A read ERROR is Unknown, which ALLOWS (fail-open on introspection, warned): we refuse on evidence, never on
///   the absence of evidence.
/// - A KNOWN withdraw-capable key REFUSES (logged at `error!`) and the account is NOT RUNNING — the same outcome
///   absent credentials produce — unless `allow_override` (`flags.allow_withdraw_keys`).
///
/// `key_words` names the key in the refusal ("this API key"; hyperliquid's "the account's own key"), `cure` says
/// what to do instead ("Re-issue a trade-only key"; "Sign with an approved API wallet instead"). With binance's
/// two the refusal's words are its former gate's byte for byte.
pub fn withdraw_gate_before_start(
    venue: &str,
    account: &str,
    mainnet: bool,
    allow_override: bool,
    key_words: &str,
    cure: &str,
    fetch_permissions: impl FnOnce() -> Result<KeyPermissions, String>,
) -> CredentialWithdrawGate {
    if !mainnet {
        return CredentialWithdrawGate::Allow;
    }
    let perms = fetch_permissions().unwrap_or_else(|e| {
        tracing::warn!(
            venue,
            account,
            error = %e,
            "key-permission introspection failed → permissions Unknown (fail-open); the withdraw \
             gate refuses only a KNOWN withdraw-capable key"
        );
        KeyPermissions::UNKNOWN
    });
    let verdict = withdraw_gate(&perms, allow_override);
    if verdict == CredentialWithdrawGate::Refuse {
        tracing::error!(
            venue,
            account,
            "⚠ REFUSING to arm {venue} LIVE: {key_words} can WITHDRAW. The account does NOT RUN \
             (the same outcome absent credentials produce). {cure}, or \
             `vike-cli config set flags.allow_withdraw_keys true` and restart to override."
        );
    }
    verdict
}

/// bybit's `GET /v5/user/query-api` answer → [`KeyPermissions`]. `can_withdraw` is whether
/// `result.permissions.Wallet` holds the EXACT element `"Withdraw"` (`None` without that list); `can_trade` is
/// `readOnly == 0` (a read-write key). A `retCode` other than `0` is the read's error.
pub fn bybit_key_permissions(answer: &Value) -> Result<KeyPermissions, String> {
    if answer["retCode"] != 0 {
        return Err(format!("bybit query-api: retCode {} {}", answer["retCode"], answer["retMsg"]));
    }
    let result = &answer["result"];
    let wallet = result["permissions"]["Wallet"].as_array();
    Ok(KeyPermissions {
        can_withdraw: wallet.map(|list| list.iter().any(|p| p.as_str() == Some("Withdraw"))),
        can_trade: result["readOnly"].as_i64().map(|read_only| read_only == 0),
        ip_restricted: None,
    })
}

/// okx's `GET /api/v5/account/config` answer → [`KeyPermissions`]: `data[0].perm` is a comma-separated token
/// list, and `can_withdraw` / `can_trade` are whether it holds the EXACT token `withdraw` / `trade` (`None`
/// without a `perm`). A `code` other than `"0"` is the read's error.
pub fn okx_key_permissions(answer: &Value) -> Result<KeyPermissions, String> {
    if answer["code"] != "0" {
        return Err(format!("okx account/config: code {} {}", answer["code"], answer["msg"]));
    }
    let perm = answer["data"][0]["perm"].as_str();
    let holds = |token: &str| perm.map(|p| p.split(',').any(|t| t.trim() == token));
    Ok(KeyPermissions {
        can_withdraw: holds("withdraw"),
        can_trade: holds("trade"),
        ip_restricted: None,
    })
}

/// hyperliquid's `POST /info {"type":"userRole"}` answer for the SIGNING address → [`KeyPermissions`]: `user`
/// means the signer IS the account's own key, which signs a withdrawal like any other action (`can_withdraw`
/// `Some(true)`); `agent` is an approved API wallet, which cannot withdraw (`Some(false)`); any other role
/// (`missing`, `vault`, `subAccount`, one this build does not know) is Unknown.
#[must_use]
pub fn hyperliquid_key_permissions(answer: &Value) -> KeyPermissions {
    let can_withdraw = match answer["role"].as_str() {
        Some("user") => Some(true),
        Some("agent") => Some(false),
        _ => None,
    };
    KeyPermissions { can_withdraw, ..KeyPermissions::UNKNOWN }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use vike_log::capture::captured;

    /// A KNOWN withdraw-capable key is refused (no override).
    #[test]
    fn refuses_withdraw_enabled_key() {
        let perms = KeyPermissions { can_withdraw: Some(true), ..KeyPermissions::UNKNOWN };
        assert_eq!(withdraw_gate(&perms, false), CredentialWithdrawGate::Refuse);
    }

    /// A trade-only key (withdraw KNOWN-false) arms.
    #[test]
    fn allows_trade_only_key() {
        let perms = KeyPermissions {
            can_withdraw: Some(false),
            can_trade: Some(true),
            ip_restricted: Some(true),
        };
        assert_eq!(withdraw_gate(&perms, false), CredentialWithdrawGate::Allow);
    }

    /// The override bypasses the refusal for a withdraw-capable key.
    #[test]
    fn override_bypasses_refusal() {
        let perms = KeyPermissions { can_withdraw: Some(true), ..KeyPermissions::UNKNOWN };
        assert_eq!(withdraw_gate(&perms, true), CredentialWithdrawGate::Allow);
    }

    /// OFF/DEFAULT byte-identical invariant: a venue that reports Unknown (no endpoint / not yet a
    /// probe row) is NEVER refused with no override — so nothing changes for any venue until an
    /// adapter opts in AND its key is KNOWN withdraw-capable. `UNKNOWN` == `default()`.
    #[test]
    fn unknown_never_refuses_and_is_the_default() {
        assert_eq!(KeyPermissions::UNKNOWN, KeyPermissions::default());
        assert_eq!(withdraw_gate(&KeyPermissions::UNKNOWN, false), CredentialWithdrawGate::Allow);
        // Even a `None` withdraw with the other fields KNOWN stays Allow.
        let partial = KeyPermissions {
            can_withdraw: None,
            can_trade: Some(true),
            ip_restricted: Some(false),
        };
        assert_eq!(withdraw_gate(&partial, false), CredentialWithdrawGate::Allow);
    }

    const WITHDRAW_CAPABLE: KeyPermissions =
        KeyPermissions { can_withdraw: Some(true), can_trade: None, ip_restricted: None };

    /// The gate on mainnet, with binance's words: the verdict is whatever answer `fetched` is.
    fn mainnet_gate(
        allow_override: bool,
        fetched: Result<KeyPermissions, String>,
    ) -> CredentialWithdrawGate {
        withdraw_gate_before_start(
            "binance",
            "DEFAULT",
            true,
            allow_override,
            "this API key",
            "Re-issue a trade-only key",
            || fetched,
        )
    }

    /// The answer a venue's capture smoke recorded: the fixture's one frame.
    fn captured_answer(fixture: &str) -> Value {
        let file: Value = serde_json::from_str(fixture).expect("the fixture is JSON");
        file["frames"][0].clone()
    }

    fn bybit_demo() -> Value {
        captured_answer(include_str!(
            "../../bridges/bybit/tests/fixtures/captured/query_api_demo.json"
        ))
    }

    fn okx_demo() -> Value {
        captured_answer(include_str!(
            "../../bridges/okx/tests/fixtures/captured/account_config_demo.json"
        ))
    }

    /// Off mainnet the read is never made, and the start arms.
    #[test]
    fn off_mainnet_nothing_is_read_and_the_start_arms() {
        let reads = Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            Ok(WITHDRAW_CAPABLE)
        };
        let verdict = withdraw_gate_before_start("bybit", "a", false, false, "key", "x", read);
        assert_eq!((verdict, reads.get()), (CredentialWithdrawGate::Allow, 0));
    }

    /// A read error is Unknown: the start arms (fail-open), and the warning names the venue, the account and
    /// the error.
    #[test]
    fn a_read_error_arms_and_is_warned() {
        let (verdict, events) = captured(|| mainnet_gate(false, Err("timed out".to_string())));
        assert_eq!(verdict, CredentialWithdrawGate::Allow);
        let warned: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::WARN).collect();
        assert_eq!(warned.len(), 1, "{events:?}");
        assert_eq!(warned[0].field("venue"), Some("binance"));
        assert_eq!(warned[0].field("account"), Some("DEFAULT"));
        assert_eq!(warned[0].field("error"), Some("timed out"));
        assert!(events.iter().all(|e| e.level != tracing::Level::ERROR), "no refusal: {events:?}");
    }

    /// A KNOWN withdraw-capable key refuses; the override arms it.
    #[test]
    fn a_withdraw_key_refuses_and_the_override_arms_it() {
        assert_eq!(mainnet_gate(false, Ok(WITHDRAW_CAPABLE)), CredentialWithdrawGate::Refuse);
        assert_eq!(mainnet_gate(true, Ok(WITHDRAW_CAPABLE)), CredentialWithdrawGate::Allow);
    }

    /// The refusal names the venue, the account, the cure and the override; with binance's words its sentence
    /// is the one binance's own gate logged, byte for byte, and hyperliquid's words read in place.
    #[test]
    fn the_refusal_line_names_venue_account_cure_and_override() {
        let (_, events) = captured(|| mainnet_gate(false, Ok(WITHDRAW_CAPABLE)));
        let refused: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::ERROR).collect();
        assert_eq!(refused.len(), 1, "{events:?}");
        let line = refused[0];
        assert_eq!(line.target, "vike_secrets::key_permissions");
        assert_eq!(
            (line.field("venue"), line.field("account")),
            (Some("binance"), Some("DEFAULT"))
        );
        assert_eq!(
            line.message,
            "⚠ REFUSING to arm binance LIVE: this API key can WITHDRAW. The account does NOT RUN \
             (the same outcome absent credentials produce). Re-issue a trade-only key, or \
             `vike-cli config set flags.allow_withdraw_keys true` and restart to override."
        );
        let (_, events) = captured(|| {
            withdraw_gate_before_start(
                "hyperliquid",
                "alt",
                true,
                false,
                "the account's own key",
                "Sign with an approved API wallet instead",
                || Ok(hyperliquid_key_permissions(&serde_json::json!({"role": "user"}))),
            )
        });
        let line = events.iter().find(|e| e.level == tracing::Level::ERROR).expect("refused");
        assert_eq!(line.field("account"), Some("alt"));
        for words in [
            "arm hyperliquid LIVE: the account's own key can WITHDRAW",
            "Sign with an approved API wallet instead, or",
            "flags.allow_withdraw_keys",
        ] {
            assert!(line.message.contains(words), "{words:?} in {}", line.message);
        }
    }

    /// bybit's captured DEMO answer is a trade-only key, so the start arms; the same answer with `"Withdraw"`
    /// added to `Wallet` by hand (a withdraw-capable key cannot be captured on demo) refuses; an element that
    /// merely CONTAINS the word does not count; a refused read is the read's error.
    #[test]
    fn bybit_captured_answer_parses_and_withdraw_is_an_exact_wallet_element() {
        let demo = bybit_demo();
        let perms = bybit_key_permissions(&demo).expect("retCode 0");
        assert_eq!((perms.can_withdraw, perms.can_trade), (Some(false), Some(true)));
        assert_eq!(mainnet_gate(false, Ok(perms)), CredentialWithdrawGate::Allow);

        let with = |element: &str| {
            let mut answer = demo.clone();
            answer["result"]["permissions"]["Wallet"]
                .as_array_mut()
                .expect("Wallet")
                .push(element.into());
            bybit_key_permissions(&answer).expect("retCode 0")
        };
        assert_eq!(with("Withdraw").can_withdraw, Some(true));
        assert_eq!(mainnet_gate(false, Ok(with("Withdraw"))), CredentialWithdrawGate::Refuse);
        assert_eq!(
            with("WithdrawHistory").can_withdraw,
            Some(false),
            "a substring is no permission"
        );

        let mut no_wallet = demo.clone();
        no_wallet["result"]["permissions"] = serde_json::json!({});
        assert_eq!(bybit_key_permissions(&no_wallet).expect("retCode 0").can_withdraw, None);
        let expired = serde_json::json!({"retCode": 33004, "retMsg": "Your api key has expired."});
        assert!(bybit_key_permissions(&expired).unwrap_err().contains("33004"));
    }

    /// okx's captured DEMO answer (`perm` `read_only,trade`) arms; the token `withdraw` added by hand refuses;
    /// a token that merely CONTAINS the word does not count; a non-zero `code` is the read's error.
    #[test]
    fn okx_captured_answer_parses_and_withdraw_is_an_exact_perm_token() {
        let demo = okx_demo();
        let perms = okx_key_permissions(&demo).expect("code 0");
        assert_eq!((perms.can_withdraw, perms.can_trade), (Some(false), Some(true)));
        assert_eq!(mainnet_gate(false, Ok(perms)), CredentialWithdrawGate::Allow);

        let with = |perm: &str| {
            let mut answer = demo.clone();
            answer["data"][0]["perm"] = perm.into();
            okx_key_permissions(&answer).expect("code 0")
        };
        assert_eq!(with("read_only,trade,withdraw").can_withdraw, Some(true));
        assert_eq!(
            mainnet_gate(false, Ok(with("read_only,trade,withdraw"))),
            CredentialWithdrawGate::Refuse
        );
        assert_eq!(with("read_only,trade,withdraw_history").can_withdraw, Some(false));
        assert_eq!(with("read_only").can_trade, Some(false));

        let refused = serde_json::json!({"code": "50113", "msg": "Invalid Sign", "data": []});
        assert!(okx_key_permissions(&refused).unwrap_err().contains("50113"));
    }

    /// hyperliquid's `userRole` of the signer: `user` (the account's own key) can withdraw and refuses, `agent`
    /// (an approved API wallet) cannot and arms, and every other role is Unknown, which arms.
    #[test]
    fn hyperliquid_user_refuses_and_agent_arms() {
        let role = |answer: Value| hyperliquid_key_permissions(&answer).can_withdraw;
        assert_eq!(role(serde_json::json!({"role": "user"})), Some(true));
        assert_eq!(
            role(serde_json::json!({"role": "agent", "data": {"user": "0xMASTER"}})),
            Some(false)
        );
        for other in ["missing", "vault", "subAccount", "multiSigUser"] {
            assert_eq!(role(serde_json::json!({ "role": other })), None, "{other}");
        }
        assert_eq!(role(serde_json::json!({})), None);
        let user = hyperliquid_key_permissions(&serde_json::json!({"role": "user"}));
        assert_eq!(mainnet_gate(false, Ok(user)), CredentialWithdrawGate::Refuse);
    }
}
