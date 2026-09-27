//! `settlement` — the **post-trade settlement cluster**: everything that happens to a Polymarket
//! position AFTER the CLOB stops mattering. A market resolves on-chain, the local book must
//! flatten at its payout, and winning tokens must be redeemed for collateral — none of which any
//! CLOB endpoint reports as a fill. Nine modules, one contract, stated here ONCE (this header
//! replaces the former per-module disclaimers in lib.rs; the vike-hyperliquid `signing/` precedent
//! for a directory-grouped cluster).
//!
//! ## The cluster contract: opt-in, default OFF, NO composition-root wiring today
//!
//! No composition root constructs anything here: neither [`crate::mount`] nor any consumer crate
//! (vike-mount / vike-run / vike-app / vike-tradehub) spawns a poller/watcher/oracle from this
//! cluster — the only drivers are this crate's own tests and `#[ignore]`d live smokes. The one
//! production touch point is [`crate::recon_client`]'s OPTIONAL oracle seam
//! (`PolymarketReconClient::with_chain_oracle`, default `None`), which nothing calls today. A
//! default build/run therefore never opens a Polygon RPC socket, never posts to the gasless
//! relayer, and is byte-identical to the crate before this cluster existed.
//!
//! Three independent env gates, one per driver — each the EXACT string `"1"`, the
//! `VIKE_RECONCILE` idiom, never a fuzzy truthy parse:
//!
//! - [`chain`] — `POLY_CHAIN_WATCH=1` ([`chain::chain_watch_enabled`], [`chain::CHAIN_WATCH_ENV`]):
//!   the read-only Polygon on-chain settlement watcher/oracle — resolution + redemption truth,
//!   closing the `/positions.redeemable`-is-NOT-a-winner-flag blind spot. Never signs, never
//!   sends a transaction.
//! - [`resolve`] — `VIKE_PM_RESOLVE=1` ([`resolve::pm_resolve_enabled`]): the condition-resolution
//!   watchlist that emits the terminal LOCAL settlement fill flattening a resolved position in our
//!   own book. Moves no money. ⚠ Like `auto_redeem` below, it ALSO performs read-only Polygon RPC:
//!   [`resolve::ResolvePoller::spawn`] prices every settlement from the CTF's own
//!   `payoutNumerators` ([`resolve::PayoutSource::Chain`], the default) because
//!   `/positions.redeemable` is a RESOLUTION flag and settling off it books a fabricated profit on
//!   every losing leg. It does NOT require `POLY_CHAIN_WATCH=1` — that gate is the log-scanning
//!   watcher THREAD, not payout truth, and this poller owns its own oracle. Unreachable RPC ⇒ it
//!   settles nothing (fail closed) rather than falling back to the flag.
//! - [`auto_redeem`] — `POLY_AUTO_REDEEM=1` ([`auto_redeem::auto_redeem_enabled`]) AND credentials
//!   present, with the `POLY_REDEEM_HALT` kill switch (env present with ANY value, or the halt
//!   file) checked every tick: the unattended poller that moves the REAL on-chain money via the
//!   gasless relayer, at-most-once through [`redeem_ledger`]. ⚠ Since the redeem-confirmation fix
//!   this driver ALSO performs read-only Polygon RPC (`eth_getTransactionReceipt`) through
//!   [`redeem_confirm`], because a relayer HTTP 2xx is not proof that a redemption happened —
//!   enabling it therefore requires reachable RPC. It does NOT require `POLY_CHAIN_WATCH=1`: the
//!   confirmer owns its own client and is independent of the watcher gate.
//!
//! The other six are the UNGATED pure building blocks those drivers compose — verified calldata +
//! relayer wire ([`redeem`], [`redeem_relayer`], [`split_merge`]), data-api position discovery
//! ([`positions`]), the on-chain redemption confirmer ([`redeem_confirm`]), and the at-most-once
//! idempotency ledger ([`redeem_ledger`]). Nothing in them performs I/O at construction; only the
//! gated drivers above ever call them outside tests.
//!
//! Pure `mod`-path move: every pre-move import path still resolves — lib.rs re-exports each module
//! at the crate root (`vike_polymarket::chain::…`, `crate::redeem::…`, …) plus all the flat item
//! re-exports, so in-crate callers, tests/, and consumer crates are untouched.

pub mod auto_redeem;
pub mod chain;
pub mod positions;
pub(crate) mod redeem;
pub mod redeem_confirm;
pub(crate) mod redeem_ledger;
pub mod redeem_relayer;
pub mod resolve;
pub mod split_merge;
