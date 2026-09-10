//! ⚠ **SECURITY-CRITICAL** — the confirm-token contract: single use, 60 s expiry, and binding to
//! BOTH the exact command previewed and the chat that previewed it.
//!
//! A bug here means a confirmation fires twice, or fires something it never previewed. See the
//! module doc of [`crate::telegram`] for the contract this file enforces.
//!
//! # The invariant, stated so it cannot evaporate quietly
//!
//! "A Telegram-originated order cannot be placed twice" rests on **two independent mechanisms**,
//! and only one of them lives in this file:
//!
//! 1. the at-most-once [`UpdateLedger`](super::UpdateLedger), which persists Telegram's `update_id`
//!    high-water mark and refuses to arm the channel when it cannot be written; and
//! 2. **this store's properties**, which are why a `/confirm` replayed out of Telegram's queue
//!    after a restart executes nothing.
//!
//! Those properties are NOT emergent conveniences to be read off the implementation — each is gated
//! by a test, so a change that removes one fails CI instead of silently removing a guarantee:
//!
//! | Property | Enforced by | Gated by |
//! |---|---|---|
//! | **unpredictable** — 8 CSPRNG bytes, never a counter, never a clock | `deps.rs`'s `mint_hex` (⚠ see below) | `deps.rs`'s `every_bit_of_a_minted_token_varies_the_anti_counter_gate` |
//! | **single-use** — removed BEFORE the caller executes | [`PendingConfirms::take`] | `a_token_fires_at_most_once` |
//! | **expiring** — dropped past [`CONFIRM_WINDOW_MS`] | [`PendingConfirms::prune_expired`] + `tick.rs`'s post-take window check | `expiry_drops_past_the_window_and_keeps_the_boundary` |
//! | **chat-bound** — and a wrong-chat attempt does not BURN it | [`PendingConfirms::take`]'s pre-removal check | `a_token_is_chat_bound_and_survives_another_chats_attempt` |
//! | **command-bound** — executes what it previewed, never a re-parse | [`Pending::cmd`], stored at preview time | `take_returns_the_command_that_was_previewed` |
//! | **memory-only** — a restart carries nothing over | no persistence: a plain `BTreeMap`, built fresh per poller thread | `a_fresh_store_carries_nothing_across_a_restart` + `the_confirm_store_is_not_serializable` |
//!
//! ⚠ **Unpredictability is enforced one file away**, in `deps.rs`'s `mint_hex` — which
//! [`crate::telegram`]'s layout table classifies as *supporting*, not security-critical. So a change
//! that weakens the token minter does NOT announce itself on the changed-file list the way a change
//! to this file does. Its gate lives beside the minter it protects; this row exists so the whole
//! contract is legible from the file that owns the rest of it.

use std::collections::BTreeMap;

use vike_tradehub_client::wire::WireCommand;

use super::{CONFIRM_WINDOW_MS, MAX_PENDING};

// Compile-time bounds on the two constants this file's contract rests on. A confirmation token
// authorizes a REAL order, so its window must stay short and the store must stay bounded. RANGES,
// not exact values: a deliberate tweak stays free, while "expiry was effectively removed"
// (`CONFIRM_WINDOW_MS = i64::MAX`) and "the bound was lifted" do not compile.
const _: () = assert!(
    CONFIRM_WINDOW_MS > 0 && CONFIRM_WINDOW_MS <= 300_000,
    "CONFIRM_WINDOW_MS must stay a small POSITIVE number of milliseconds"
);
const _: () = assert!(MAX_PENDING > 0 && MAX_PENDING <= 64, "MAX_PENDING must stay bounded");

/// One previewed-but-unconfirmed command, held in memory ONLY (never persisted — see the module
/// doc: that is what makes a restart-replayed `/confirm` a no-op).
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub token: String,
    pub cmd: WireCommand,
    /// The audit rationale ([`confirm_reason`](super::confirm_reason) over the ORIGINAL
    /// instruction's text).
    pub reason: String,
    /// The chat that previewed it — a token is bound to its chat as well as its command.
    pub chat_id: i64,
    /// The USER who previewed it, for attribution only.
    ///
    /// ⚠ Deliberately NOT part of the token binding — [`PendingConfirms::take`] compares the chat
    /// and not this. Binding here would mean an operator could not confirm a colleague's preview,
    /// which is a change to who may authorize an order and is not one to make silently; the
    /// per-user allowlist ([`TelegramConfig::allows_user`](super::TelegramConfig::allows_user)) is
    /// the opt-in that tightens that, deployment by deployment. What this field buys is that the
    /// audit line can say both names when they differ — see
    /// [`confirmed_by`](super::confirmed_by).
    pub from_id: i64,
    pub issued_ms: i64,
}

/// The bounded token store. Keyed by token so a confirm is an exact lookup; [`Self::take`] REMOVES
/// before the caller executes, which is what makes a token single-use even against a duplicated
/// message.
#[derive(Debug, Default)]
pub struct PendingConfirms {
    by_token: BTreeMap<String, Pending>,
}

impl PendingConfirms {
    pub fn len(&self) -> usize {
        self.by_token.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_token.is_empty()
    }

    pub fn contains(&self, token: &str) -> bool {
        self.by_token.contains_key(token)
    }

    /// Store a preview, evicting the OLDEST entry when the store is full.
    pub fn insert(&mut self, pending: Pending) {
        if self.by_token.len() >= MAX_PENDING {
            // Bind first: the `values()` iterator borrows the map, and an `if let` scrutinee's
            // temporaries live to the end of its block in edition 2021 — which would collide with
            // the `remove` below.
            let oldest =
                self.by_token.values().min_by_key(|p| p.issued_ms).map(|p| p.token.clone());
            if let Some(token) = oldest {
                self.by_token.remove(&token);
            }
        }
        self.by_token.insert(pending.token.clone(), pending);
    }

    /// Consume a token, but only for the chat that issued it. Removal happens BEFORE the caller
    /// executes, so a token can never fire twice — the single-use property.
    ///
    /// An EXPIRED entry is still returned here (and consumed); the caller checks the window and
    /// reports the expiry distinctly, which is more useful to an operator than "unknown token".
    pub fn take(&mut self, token: &str, chat_id: i64) -> Option<Pending> {
        if self.by_token.get(token).map(|p| p.chat_id) != Some(chat_id) {
            return None;
        }
        self.by_token.remove(token)
    }

    /// Drop every entry past [`CONFIRM_WINDOW_MS`]; returns how many went. Bounds memory on a chat
    /// that previews and never confirms.
    pub fn prune_expired(&mut self, now_ms: i64) -> usize {
        let before = self.by_token.len();
        self.by_token.retain(|_, p| now_ms.saturating_sub(p.issued_ms) <= CONFIRM_WINDOW_MS);
        before - self.by_token.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT: i64 = 42;
    /// A SECOND allowlisted chat. `VIKE_TELEGRAM_ALLOWED_CHAT_IDS` is a comma-separated list, so
    /// this is an ordinary configuration — and it is the case the allowlist does NOT cover, because
    /// both chats pass it. Only [`PendingConfirms::take`]'s own check separates them.
    const OTHER_CHAT: i64 = 43;

    fn pending_at(token: &str, chat_id: i64, issued_ms: i64) -> Pending {
        Pending {
            token: token.to_string(),
            cmd: WireCommand::Cancel(format!("ORDER-{token}")),
            reason: "why".into(),
            chat_id,
            from_id: 900 + chat_id,
            issued_ms,
        }
    }

    /// SINGLE USE. [`PendingConfirms::take`] REMOVES before it returns, which is what makes a
    /// duplicated `/confirm` a no-op rather than a second order — and it holds for an EXPIRED entry
    /// too: `tick.rs` deliberately takes first and checks the window second, so a stale token is
    /// burned rather than left lying around for a later retry.
    #[test]
    fn a_token_fires_at_most_once() {
        let mut pending = PendingConfirms::default();
        pending.insert(pending_at("t", CHAT, 0));

        assert!(pending.take("t", CHAT).is_some(), "the first confirm gets the command");
        assert!(pending.take("t", CHAT).is_none(), "the second gets NOTHING — single use");
        assert!(pending.is_empty());

        // …and an entry long past its window is still consumed by the take that inspects it.
        pending.insert(pending_at("stale", CHAT, 0));
        assert!(
            pending.take("stale", CHAT).is_some(),
            "an expired entry is RETURNED (the caller reports the expiry distinctly)"
        );
        assert!(pending.is_empty(), "…and BURNED by that take, not left for a retry");
    }

    /// EXPIRY. A previewed order does not stay executable: [`PendingConfirms::prune_expired`] runs
    /// at the end of every poll pass. The boundary is asserted from BOTH sides so a `>` that became
    /// a `>=` (or the reverse) is caught rather than rounded over.
    #[test]
    fn expiry_drops_past_the_window_and_keeps_the_boundary() {
        let mut pending = PendingConfirms::default();
        pending.insert(pending_at("t", CHAT, 1_000));

        assert_eq!(pending.prune_expired(1_000 + CONFIRM_WINDOW_MS), 0, "AT the window: kept");
        assert!(pending.contains("t"));

        assert_eq!(pending.prune_expired(1_000 + CONFIRM_WINDOW_MS + 1), 1, "one ms past: dropped");
        assert!(pending.is_empty(), "an un-confirmed preview cannot linger");
        // (That the window is SHORT, and the store bounded, is gated at compile time — see the
        // `const _: () = assert!(…)` pair above the impl.)
    }

    /// CHAT BINDING, in the configuration the allowlist cannot help with: TWO allowlisted chats.
    /// `tick.rs` drops an UNLISTED chat before dispatch ever runs, so this check is the only thing
    /// standing between two legitimate operators' tokens.
    ///
    /// The second assertion is the load-bearing one: `take` compares the chat BEFORE removing, so a
    /// wrong-chat attempt is not a way to burn somebody else's pending order.
    #[test]
    fn a_token_is_chat_bound_and_survives_another_chats_attempt() {
        let mut pending = PendingConfirms::default();
        pending.insert(pending_at("t", CHAT, 0));

        assert!(pending.take("t", OTHER_CHAT).is_none(), "another chat cannot spend it");
        assert!(pending.contains("t"), "…and the failed attempt did NOT consume it");
        assert!(pending.take("t", CHAT).is_some(), "the issuing chat still can");
    }

    /// COMMAND BINDING. The stored [`Pending::cmd`] is the command resolved at PREVIEW time — the
    /// one the operator read — so a confirm can never execute a re-parse of the `/confirm` message
    /// or a neighbouring preview.
    #[test]
    fn take_returns_the_command_that_was_previewed() {
        let mut pending = PendingConfirms::default();
        pending.insert(pending_at("a", CHAT, 0));
        pending.insert(pending_at("b", CHAT, 1));

        let taken = pending.take("a", CHAT).expect("token a resolves");
        assert_eq!(taken.cmd, WireCommand::Cancel("ORDER-a".into()), "verbatim, never b's");
        assert_eq!(taken.chat_id, CHAT);
        assert!(pending.contains("b"), "b's preview is untouched by a's confirm");
    }

    /// MEMORY-ONLY, behaviourally: the store the poller thread builds is
    /// [`PendingConfirms::default`] — the same expression [`crate::telegram::spawn`] uses — and it
    /// knows nothing about any token issued before it existed.
    ///
    /// This is what makes a first-run backlog harmless: a day-old `/confirm` replayed out of
    /// Telegram's queue after a restart finds no entry and executes nothing.
    #[test]
    fn a_fresh_store_carries_nothing_across_a_restart() {
        let token = {
            let mut before_restart = PendingConfirms::default();
            before_restart.insert(pending_at("survivor", CHAT, 0));
            assert!(before_restart.contains("survivor"));
            "survivor"
        };

        let mut after_restart = PendingConfirms::default();
        assert!(after_restart.is_empty(), "a fresh store starts empty — nothing is loaded");
        assert!(!after_restart.contains(token), "the pre-restart token is unknown");
        assert!(after_restart.take(token, CHAT).is_none(), "…and unusable");
    }

    /// MEMORY-ONLY, structurally: the TRIPWIRE on the "one change away" risk.
    ///
    /// Persisting this store means first making it serializable, so a `#[derive(Serialize)]` or
    /// `#[derive(Deserialize)]` on either type fails HERE — at the first step of the change, while
    /// it is still a review conversation rather than a shipped behaviour. It is deliberately not a
    /// claim that nothing can ever write a token to disk (a hand-rolled `write!` would not name
    /// serde); it is a claim that the ordinary way to do it trips a gate.
    #[test]
    fn the_confirm_store_is_not_serializable() {
        use serde_probe::{DeFallback, DeSpecific, Probe, SerFallback, SerSpecific};
        use std::marker::PhantomData;

        // POSITIVE CONTROL FIRST. A probe that answered `false` unconditionally — a broken
        // fallback, a bound that stopped resolving, an import that stopped being in scope — would
        // pass every assertion below while checking NOTHING. That is the exact shape of a
        // declaration-pinning gate that stays green as the thing it pins drifts away, so the probe
        // is made to prove itself on a type that certainly IS serde before it is believed.
        assert!(Probe::<String>(PhantomData).serializable(), "the Serialize probe is broken");
        assert!(Probe::<String>(PhantomData).deserializable(), "the Deserialize probe is broken");

        assert!(!Probe::<PendingConfirms>(PhantomData).serializable(), "{}", WHY);
        assert!(!Probe::<PendingConfirms>(PhantomData).deserializable(), "{}", WHY);
        assert!(!Probe::<Pending>(PhantomData).serializable(), "{}", WHY);
        assert!(!Probe::<Pending>(PhantomData).deserializable(), "{}", WHY);
    }

    const WHY: &str = "a confirm token became serializable — persisting PendingConfirms would let a \
                       restart-replayed /confirm execute an order the operator confirmed before the \
                       restart. If this is intentional, the at-most-once argument in this file's \
                       module doc has to be rewritten first.";

    /// A compile-time probe for "does `T` implement `serde::Serialize`?" and, independently, "…
    /// `serde::de::DeserializeOwned`?".
    ///
    /// Autoref specialization, the standard stable-Rust idiom: method lookup tries the receiver
    /// `Probe<T>` BY VALUE first — which is where the trait-BOUNDED impls live — and only autorefs
    /// to `&Probe<T>`, finding the unconditional fallback, when no bounded candidate applies. So
    /// `Probe::<T>(PhantomData).serializable()` is `true` exactly when `T: Serialize`, with no
    /// nightly feature and no added dependency.
    ///
    /// The two bounds get their OWN trait pair on purpose: probing `Serialize + DeserializeOwned`
    /// in one impl would answer `false` for a type that derived only one of them — which is exactly
    /// the half-finished change this tripwire exists to catch.
    mod serde_probe {
        use std::marker::PhantomData;

        pub struct Probe<T>(pub PhantomData<T>);

        /// SPECIFIC (`T: Serialize`) — reached first, by value.
        pub trait SerSpecific {
            fn serializable(self) -> bool;
        }

        impl<T: serde::Serialize> SerSpecific for Probe<T> {
            fn serializable(self) -> bool {
                true
            }
        }

        /// FALLBACK for every `T` — reachable only through the autoref step.
        pub trait SerFallback {
            fn serializable(self) -> bool;
        }

        impl<T> SerFallback for &Probe<T> {
            fn serializable(self) -> bool {
                false
            }
        }

        /// SPECIFIC (`T: DeserializeOwned`) — the same shape for the READ-back half.
        pub trait DeSpecific {
            fn deserializable(self) -> bool;
        }

        impl<T: serde::de::DeserializeOwned> DeSpecific for Probe<T> {
            fn deserializable(self) -> bool {
                true
            }
        }

        /// FALLBACK for every `T`.
        pub trait DeFallback {
            fn deserializable(self) -> bool;
        }

        impl<T> DeFallback for &Probe<T> {
            fn deserializable(self) -> bool {
                false
            }
        }
    }

    #[test]
    fn pending_store_is_bounded_and_chat_bound() {
        let mut pending = PendingConfirms::default();
        let make = |n: usize| Pending {
            token: format!("t{n}"),
            cmd: WireCommand::Cancel(format!("c{n}")),
            reason: "why".into(),
            chat_id: 42,
            from_id: 7,
            issued_ms: n as i64,
        };
        for n in 0..MAX_PENDING + 3 {
            pending.insert(make(n));
        }
        assert_eq!(pending.len(), MAX_PENDING, "bounded");
        assert!(!pending.contains("t0"), "the OLDEST entries were evicted");
        assert!(pending.contains(&format!("t{}", MAX_PENDING + 2)));

        // A token is bound to its chat: another chat cannot spend it.
        assert!(pending.take(&format!("t{}", MAX_PENDING + 2), 43).is_none());
        assert!(pending.take(&format!("t{}", MAX_PENDING + 2), 42).is_some());
        // …and taking removes it (single use).
        assert!(pending.take(&format!("t{}", MAX_PENDING + 2), 42).is_none());
    }
}
