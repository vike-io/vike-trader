//! **Is this account's API-wallet (agent) approval still alive?** — asked of the venue's PUBLIC `/info`,
//! with no credential in reach.
//!
//! An agent wallet is approved by its master for at most 180 days (`validUntil`, epoch ms). When the
//! approval lapses the venue answers `userRole` `missing` for the signer and rejects EVERY order with
//! `User or API Wallet 0x... does not exist`; nothing on this side changes, so the only symptom used to be
//! the per-order rejections (the testnet approval lapsed 2026-10-10 and nobody was told until the orders
//! failed). Two keyless reads settle it:
//!
//! * `extraAgents` of the MASTER lists the approved agents with their `validUntil` (weight 20);
//! * `userRole` of the SIGNER says `agent` / `missing` / `user` (weight 60, see [`crate::feeds::user_role`]).
//!
//! **This module only ANSWERS.** [`check`] returns an [`AgentStatus`]; logging, the operator alert and the
//! hourly schedule are the gatewayhub's (`crates/vike-gatewayhub/src/trade/adapters/agent_watch.rs`), and
//! nothing here changes a trading decision, blocks a start or halts anything (decision 0013: degrade, do
//! not refuse). An answer the venue did not give is [`AgentStatus::CouldNotCheck`], never an alarm.
//!
//! # No credential leaves this module
//!
//! [`subject_of`] derives the signer's public ADDRESS from the key and drops the key; an
//! [`AgentSubject`] holds addresses only, so the watcher that keeps one for hours holds no secret and every
//! line it logs may name it. (Agent -> master is public information anyway, see `user_role`'s module doc.)
//!
//! # The weight
//!
//! 80 per check, on a transport with its OWN fresh gate (the adapter's shared window is not reachable from
//! the gatewayhub): once at a start and once an hour per account, against 1200 per minute per IP.

use serde_json::Value;
use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;

use crate::base::config::{self, Network};
use crate::base::transport::HyperliquidTransport;
use crate::feeds::user_role::{UserRole, parse_user_role, user_role_body};

/// One day in epoch milliseconds.
pub const DAY_MS: i64 = 86_400_000;
/// An approval with this much life or less left is [`AgentStatus::ExpiresSoon`].
pub const SOON_MS: i64 = 14 * DAY_MS;
/// An approval with this much life or less left is [`AgentStatus::ExpiresSoon`] with `urgent` set.
pub const URGENT_MS: i64 = 3 * DAY_MS;

/// WHO to ask about: public addresses only, safe to keep, log and compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSubject {
    /// The network the account's tier reaches (`live` is mainnet).
    pub network: Network,
    /// The signing key's own address (lowercased `0x...`), derived from the key.
    pub signer: String,
    /// The account's configured `ACCOUNT_ADDRESS` (lowercased), when it states one.
    pub master: Option<String>,
}

/// One row of `extraAgents`: an approved agent and when its approval ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGrant {
    /// The agent's address, lowercased.
    pub address: String,
    /// `validUntil`, epoch ms; `None` when the row states none.
    pub valid_until_ms: Option<i64>,
}

/// What the venue says about the account's ability to sign.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// Approved, with more than [`SOON_MS`] left (`None`: approved and the row states no end).
    Ok { valid_until_ms: Option<i64> },
    /// The key is its own account (a master key): no approval exists or is needed.
    MasterKey,
    /// Approved, but the approval ends within [`SOON_MS`]; `urgent` is within [`URGENT_MS`].
    ExpiresSoon { valid_until_ms: i64, urgent: bool },
    /// ⚠ The approval's `validUntil` has passed: the venue rejects every order of this account.
    Expired { valid_until_ms: i64 },
    /// ⚠ The venue knows no approval of this signer for the account (never approved, revoked, or lapsed and
    /// pruned): the venue rejects every order of this account.
    Missing,
    /// The venue did not answer (or answered a shape this build does not read). Never an alarm.
    CouldNotCheck(String),
}

impl AgentStatus {
    /// Does an operator have to act (now or soon)?
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.rank() > 0
    }

    /// Is the account unable to sign RIGHT NOW (the venue rejects every order)?
    #[must_use]
    pub fn cannot_sign(&self) -> bool {
        matches!(self, AgentStatus::Expired { .. } | AgentStatus::Missing)
    }

    /// The severity class, `0` = nothing to say: the watcher re-alerts at once when it RISES.
    #[must_use]
    pub fn rank(&self) -> u8 {
        match self {
            AgentStatus::Ok { .. } | AgentStatus::MasterKey | AgentStatus::CouldNotCheck(_) => 0,
            AgentStatus::ExpiresSoon { urgent: false, .. } => 1,
            AgentStatus::ExpiresSoon { urgent: true, .. } => 2,
            AgentStatus::Expired { .. } | AgentStatus::Missing => 3,
        }
    }

    /// One sentence, no addresses: what is wrong and how long it has (`now_ms` is the clock).
    #[must_use]
    pub fn headline(&self, now_ms: i64) -> String {
        match self {
            AgentStatus::Ok { valid_until_ms: Some(v) } => {
                format!("the API-wallet approval is valid until {}", utc(*v))
            }
            AgentStatus::Ok { valid_until_ms: None } => "the API-wallet approval is valid".into(),
            AgentStatus::MasterKey => "the key is a master account (no API-wallet approval)".into(),
            AgentStatus::ExpiresSoon { valid_until_ms, .. } => format!(
                "the API-wallet approval EXPIRES in {} (at {}); once it lapses the venue rejects every order",
                left(valid_until_ms - now_ms),
                utc(*valid_until_ms)
            ),
            AgentStatus::Expired { valid_until_ms } => format!(
                "the API-wallet approval EXPIRED at {} ({} ago): the venue rejects EVERY order of this account",
                utc(*valid_until_ms),
                left(now_ms - valid_until_ms)
            ),
            AgentStatus::Missing => "the venue knows no API-wallet approval for this signer \
                (expired, revoked or never made): the venue rejects EVERY order of this account"
                .into(),
            AgentStatus::CouldNotCheck(why) => format!("could not check the approval ({why})"),
        }
    }
}

/// The operator's cure, spelled: where to approve the signer address, per network.
#[must_use]
pub fn cure(network: Network) -> &'static str {
    match network {
        Network::Mainnet => {
            "approve this address on app.hyperliquid.xyz/API (as the master account)"
        }
        Network::Testnet => {
            "approve this address on app.hyperliquid-testnet.xyz/API (testnet: as the master account)"
        }
    }
}

fn utc(ms: i64) -> String {
    vike_model::time::epoch_ms_to_utc_timestamp(ms)
}

/// `"2d 5h"` / `"7h"` / `"under 1h"`, for a non-negative span.
fn left(span_ms: i64) -> String {
    let hours = span_ms.max(0) / 3_600_000;
    match (hours / 24, hours % 24) {
        (0, 0) => "under 1h".to_string(),
        (0, h) => format!("{h}h"),
        (d, h) => format!("{d}d {h}h"),
    }
}

/// The `/info` body listing a master's approved agents. Exposed so a test pins the wire shape.
#[must_use]
pub fn extra_agents_body(master: &str) -> Value {
    serde_json::json!({ "type": "extraAgents", "user": master })
}

/// Parse the `extraAgents` answer, PURE: an array of `{name, address, validUntil}`. A row with no address is
/// skipped; any other shape than an array is an `Err` (the caller says "could not check", not "missing").
///
/// # Errors
/// The body is not a JSON array.
pub fn parse_extra_agents(v: &Value) -> Result<Vec<AgentGrant>, String> {
    let rows = v.as_array().ok_or_else(|| "extraAgents did not answer an array".to_string())?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let address = row.get("address")?.as_str()?.trim().to_ascii_lowercase();
            let valid_until_ms = row.get("validUntil").and_then(|x| {
                x.as_i64().or_else(|| x.as_u64().and_then(|u| i64::try_from(u).ok()))
            });
            Some(AgentGrant { address, valid_until_ms })
        })
        .collect())
}

/// The status from what the venue said, PURE. `grant` is the signer's row in its master's `extraAgents`
/// (`None`: no such row, or the master was not asked).
#[must_use]
pub fn classify(
    subject: &AgentSubject,
    role: &UserRole,
    grant: Option<&AgentGrant>,
    now_ms: i64,
) -> AgentStatus {
    let by_date = |valid_until_ms: i64| {
        let remaining = valid_until_ms - now_ms;
        if remaining <= 0 {
            AgentStatus::Expired { valid_until_ms }
        } else if remaining <= SOON_MS {
            AgentStatus::ExpiresSoon { valid_until_ms, urgent: remaining <= URGENT_MS }
        } else {
            AgentStatus::Ok { valid_until_ms: Some(valid_until_ms) }
        }
    };
    match role {
        // The key is its own account, and the account configured is that one (or none is configured).
        UserRole::User if subject.master.as_deref().is_none_or(|m| m == subject.signer) => {
            AgentStatus::MasterKey
        }
        // The venue says the signer is an approved agent: the row gives the end, if it states one.
        UserRole::Agent { .. } => match grant.and_then(|g| g.valid_until_ms) {
            Some(v) => by_date(v),
            None => AgentStatus::Ok { valid_until_ms: None },
        },
        // A `user` key beside a DIFFERENT configured master, or an address the venue does not know: the
        // signer cannot sign for the master unless the master's own list says so.
        UserRole::User | UserRole::Missing => match grant.and_then(|g| g.valid_until_ms) {
            Some(v) if v <= now_ms => AgentStatus::Expired { valid_until_ms: v },
            _ => AgentStatus::Missing,
        },
        other => AgentStatus::CouldNotCheck(format!(
            "the venue classifies the signer's address as {other:?}, which is not an API wallet"
        )),
    }
}

/// Ask through `read` (a scripted double in a test, the transport in production) and classify.
/// `read` takes an `/info` body and answers the parsed reply or the failure in words.
#[must_use]
pub fn check_with(
    read: &dyn Fn(&Value) -> Result<Value, String>,
    subject: &AgentSubject,
    now_ms: i64,
) -> AgentStatus {
    let role = match read(&user_role_body(&subject.signer)) {
        Ok(v) => parse_user_role(&v),
        Err(e) => return AgentStatus::CouldNotCheck(format!("userRole: {e}")),
    };
    // Whose list holds the approval: the configured account's, else the master the venue named.
    let master = match (&subject.master, &role) {
        (Some(m), _) => Some(m.as_str()),
        (None, UserRole::Agent { master }) => Some(master.as_str()),
        _ => None,
    };
    let needs_list = match &role {
        UserRole::Agent { .. } | UserRole::Missing => true,
        // A `user` key is its own account unless the account configured is somebody else's.
        UserRole::User => master.is_some_and(|m| m != subject.signer),
        _ => false,
    };
    let grant = match master {
        Some(m) if needs_list => {
            match read(&extra_agents_body(m)).and_then(|v| parse_extra_agents(&v)) {
                Ok(rows) => rows.into_iter().find(|g| g.address == subject.signer),
                Err(e) => return AgentStatus::CouldNotCheck(format!("extraAgents: {e}")),
            }
        }
        _ => None,
    };
    classify(subject, &role, grant.as_ref(), now_ms)
}

/// [`check_with`] over the real public `/info` of `subject`'s network.
#[must_use]
pub fn check(subject: &AgentSubject, now_ms: i64) -> AgentStatus {
    let transport = HyperliquidTransport::new(subject.network);
    check_with(&|body| transport.info(body).map_err(|e| e.to_string()), subject, now_ms)
}

/// The subject of the account holding `creds` on `tier`'s network, or `None` when it holds no usable key.
/// The key is read to derive the signer's ADDRESS and is not kept.
#[must_use]
pub fn subject_of(tier: AccountTier, creds: &AccountCredentials<'_>) -> Option<AgentSubject> {
    let hl = config::load(tier, creds)?;
    let signer = crate::signing::Signer::from_private_key(&hl.private_key, hl.network).ok()?;
    Some(AgentSubject {
        network: hl.network,
        signer: signer.address().to_ascii_lowercase(),
        master: hl.account_address,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    const NOW: i64 = 1_790_000_000_000;
    const SIGNER: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const MASTER: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn subject(master: Option<&str>) -> AgentSubject {
        AgentSubject {
            network: Network::Testnet,
            signer: SIGNER.into(),
            master: master.map(str::to_string),
        }
    }

    /// The venue's HTTP, SCRIPTED: `/info` answers per `type`, and every body it was asked is kept.
    struct Script {
        role: Result<Value, String>,
        agents: Result<Value, String>,
        asked: RefCell<Vec<Value>>,
    }

    impl Script {
        fn new(role: Value, agents: Value) -> Self {
            Script { role: Ok(role), agents: Ok(agents), asked: RefCell::new(Vec::new()) }
        }
        fn read(&self, body: &Value) -> Result<Value, String> {
            self.asked.borrow_mut().push(body.clone());
            match body["type"].as_str() {
                Some("userRole") => self.role.clone(),
                Some("extraAgents") => self.agents.clone(),
                other => Err(format!("unscripted request {other:?}")),
            }
        }
        fn check(&self, s: &AgentSubject) -> AgentStatus {
            check_with(&|b| self.read(b), s, NOW)
        }
        fn types(&self) -> Vec<String> {
            self.asked.borrow().iter().map(|b| b["type"].as_str().unwrap().to_string()).collect()
        }
    }

    fn agents_valid_until(ms: i64) -> Value {
        json!([
            {"name": "other", "address": "0xcccccccccccccccccccccccccccccccccccccccc", "validUntil": NOW + 1},
            // the venue's own spelling is any case: the signer is compared lowercased
            {"name": "bot", "address": SIGNER.to_ascii_uppercase().replace("0X", "0x"), "validUntil": ms}
        ])
    }

    fn agent_role() -> Value {
        json!({"role": "agent", "data": {"user": MASTER}})
    }

    #[test]
    fn the_request_bodies_are_pinned() {
        assert_eq!(extra_agents_body("0xabc"), json!({"type": "extraAgents", "user": "0xabc"}));
    }

    #[test]
    fn ok_when_the_approval_has_more_than_14_days() {
        let s = Script::new(agent_role(), agents_valid_until(NOW + 15 * DAY_MS));
        let got = s.check(&subject(Some(MASTER)));
        assert_eq!(got, AgentStatus::Ok { valid_until_ms: Some(NOW + 15 * DAY_MS) });
        assert!(!got.needs_attention());
        assert_eq!(s.types(), ["userRole", "extraAgents"]);
        // the master's list is the configured account's, and it is asked by that address
        assert_eq!(s.asked.borrow()[1]["user"], MASTER);
    }

    #[test]
    fn expiring_in_10_days_is_soon_but_not_urgent() {
        let s = Script::new(agent_role(), agents_valid_until(NOW + 10 * DAY_MS));
        let got = s.check(&subject(Some(MASTER)));
        assert_eq!(
            got,
            AgentStatus::ExpiresSoon { valid_until_ms: NOW + 10 * DAY_MS, urgent: false }
        );
        assert!(got.needs_attention() && !got.cannot_sign());
        assert!(got.headline(NOW).contains("EXPIRES in 10d 0h"), "{}", got.headline(NOW));
    }

    #[test]
    fn expiring_in_2_days_is_urgent() {
        let s = Script::new(agent_role(), agents_valid_until(NOW + 2 * DAY_MS));
        let got = s.check(&subject(Some(MASTER)));
        assert_eq!(
            got,
            AgentStatus::ExpiresSoon { valid_until_ms: NOW + 2 * DAY_MS, urgent: true }
        );
        assert!(got.rank() > AgentStatus::ExpiresSoon { valid_until_ms: 0, urgent: false }.rank());
    }

    #[test]
    fn the_boundaries_are_inclusive_at_14_and_3_days() {
        let at = |left: i64| {
            Script::new(agent_role(), agents_valid_until(NOW + left)).check(&subject(Some(MASTER)))
        };
        assert!(matches!(at(14 * DAY_MS), AgentStatus::ExpiresSoon { urgent: false, .. }));
        assert!(matches!(at(14 * DAY_MS + 1), AgentStatus::Ok { .. }));
        assert!(matches!(at(3 * DAY_MS), AgentStatus::ExpiresSoon { urgent: true, .. }));
        assert!(matches!(at(0), AgentStatus::Expired { .. }));
    }

    /// The real shape of a lapsed approval: `userRole` says `missing`, and the master's list (when it still
    /// carries the row) shows a `validUntil` in the past.
    #[test]
    fn a_validuntil_in_the_past_is_expired() {
        let s = Script::new(json!({"role": "missing"}), agents_valid_until(NOW - 5 * DAY_MS));
        let got = s.check(&subject(Some(MASTER)));
        assert_eq!(got, AgentStatus::Expired { valid_until_ms: NOW - 5 * DAY_MS });
        assert!(got.cannot_sign());
        assert!(got.headline(NOW).contains("5d 0h ago"), "{}", got.headline(NOW));
    }

    #[test]
    fn a_missing_signer_with_no_row_is_missing() {
        let s = Script::new(json!({"role": "missing"}), json!([]));
        let got = s.check(&subject(Some(MASTER)));
        assert_eq!(got, AgentStatus::Missing);
        assert!(got.cannot_sign());
    }

    /// No `ACCOUNT_ADDRESS` and the venue does not know the key: there is no master to ask, so it is `Missing`
    /// from the one read.
    #[test]
    fn a_missing_key_with_no_configured_master_is_missing_after_one_read() {
        let s = Script::new(json!({"role": "missing"}), json!([]));
        assert_eq!(s.check(&subject(None)), AgentStatus::Missing);
        assert_eq!(s.types(), ["userRole"]);
    }

    /// With no `ACCOUNT_ADDRESS` the master is the one the venue names for the agent.
    #[test]
    fn the_master_the_venue_names_is_asked_when_none_is_configured() {
        let s = Script::new(agent_role(), agents_valid_until(NOW + 2 * DAY_MS));
        assert!(matches!(s.check(&subject(None)), AgentStatus::ExpiresSoon { urgent: true, .. }));
        assert_eq!(s.asked.borrow()[1]["user"], MASTER);
    }

    #[test]
    fn a_master_key_needs_no_approval_and_asks_one_read() {
        let s = Script::new(json!({"role": "user"}), json!([]));
        assert_eq!(s.check(&subject(None)), AgentStatus::MasterKey);
        assert_eq!(s.check(&subject(Some(SIGNER))), AgentStatus::MasterKey);
        assert_eq!(s.types(), ["userRole", "userRole"]);
    }

    /// A `user` key beside a configured master that is somebody else: it cannot sign for that master.
    #[test]
    fn a_user_key_beside_another_configured_master_is_missing() {
        let s = Script::new(json!({"role": "user"}), json!([]));
        assert_eq!(s.check(&subject(Some(MASTER))), AgentStatus::Missing);
    }

    #[test]
    fn an_api_error_is_could_not_check_never_an_alarm() {
        let mut s = Script::new(agent_role(), json!([]));
        s.role = Err("http 500".into());
        let got = s.check(&subject(Some(MASTER)));
        assert!(matches!(&got, AgentStatus::CouldNotCheck(w) if w.contains("userRole")), "{got:?}");
        assert!(!got.needs_attention() && !got.cannot_sign());

        let mut s = Script::new(agent_role(), json!([]));
        s.agents = Err("transport error".into());
        let got = s.check(&subject(Some(MASTER)));
        assert!(
            matches!(&got, AgentStatus::CouldNotCheck(w) if w.contains("extraAgents")),
            "{got:?}"
        );

        // a body that is not a list is not "missing" either
        let s = Script::new(agent_role(), json!({"error": "x"}));
        assert!(matches!(s.check(&subject(Some(MASTER))), AgentStatus::CouldNotCheck(_)));
    }

    #[test]
    fn a_role_that_is_not_an_api_wallet_is_could_not_check() {
        let s = Script::new(json!({"role": "vault"}), json!([]));
        assert!(matches!(s.check(&subject(Some(MASTER))), AgentStatus::CouldNotCheck(_)));
    }

    /// The agent the venue calls `agent` with no row in the list (or a row with no end) is approved.
    #[test]
    fn an_agent_with_no_stated_end_is_ok() {
        let s = Script::new(agent_role(), json!([]));
        assert_eq!(s.check(&subject(Some(MASTER))), AgentStatus::Ok { valid_until_ms: None });
        let s = Script::new(agent_role(), json!([{"address": SIGNER, "name": "bot"}]));
        assert_eq!(s.check(&subject(Some(MASTER))), AgentStatus::Ok { valid_until_ms: None });
    }

    #[test]
    fn the_cure_names_the_page_per_network() {
        assert!(cure(Network::Mainnet).contains("app.hyperliquid.xyz/API"));
        assert!(cure(Network::Testnet).contains("app.hyperliquid-testnet.xyz/API"));
    }

    /// The key is read for its ADDRESS and never kept: the subject, its Debug and every line the status
    /// renders hold addresses only.
    #[test]
    fn a_subject_holds_the_address_and_never_the_key() {
        use vike_secrets::account_credentials::CredentialFields;
        // The SDK vector key every signing test of this crate uses: a published test value.
        let key = "e908f86dbb4d55ac876378565aafeabc187f6690f046459397b17d9b9a19688e";
        let fields = CredentialFields::from_pairs([
            ("PRIVATE_KEY", key),
            ("ACCOUNT_ADDRESS", "0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"),
        ]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let s = subject_of(AccountTier::Demo, &creds).expect("a usable key");
        let derived = crate::signing::Signer::from_private_key(key, Network::Testnet).unwrap();
        assert_eq!(s.signer, derived.address());
        assert_eq!(s.master.as_deref(), Some(MASTER));
        assert_eq!(s.network, Network::Testnet);
        let shown = format!("{s:?} {}", AgentStatus::Missing.headline(NOW));
        assert!(!shown.contains(key), "{shown}");

        let no_key = CredentialFields::from_pairs([("ACCOUNT_ADDRESS", MASTER)]);
        assert!(
            subject_of(AccountTier::Demo, &AccountCredentials::new(Some(1), Some(&no_key), None))
                .is_none()
        );
    }
}
