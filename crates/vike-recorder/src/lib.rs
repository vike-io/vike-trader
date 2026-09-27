//! `vike-recorder` — the live-tape RECORDING LIBRARY.
//!
//! A vike customer has no prod box and no ClickHouse: the recorders behind `data.vike.io` are the
//! DATA PRODUCT's infrastructure and share no code with the terminal. If a customer wants to
//! backtest on markets they have actually watched without buying an archive, the terminal has to
//! accumulate that tape for them. That is this crate.
//!
//! Design authority: `docs/superpowers/specs/2026-08-02-live-recorder-design.md`. Its settled
//! decisions, and where each one lives:
//!
//! | decision | here |
//! |---|---|
//! | TWO daemons, not one process (§8.1) | ⚠ SUPERSEDED — see below |
//! | families are GENERAL across venues (§8.2) | [`config::Subscription::family`] — one key, Polymarket rotates its membership, a static venue filters |
//! | backfill is PER-SUBSCRIPTION (§8.3) | [`config::Backfill`] on the subscription |
//! | two-tier write path ADOPTED (§8.4) | not yet — sequenced after this skeleton, deliberately built against a real writer |
//!
//! # ⚠ THE RECORDER IS NOT ITS OWN PROCESS ANY MORE, and this file used to argue that it must be
//!
//! Ruling 10 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` (owner,
//! signed off 2026-09-10) merged the recorder daemon into the data server: *"Both are
//! data-management processes and neither is whole: the recorder OWNS venue subscriptions and has no
//! network surface at all; the data server has the wire and no feeds. One process should own venue
//! connections, the store, and serving."* The mount, the tick cadence and the whole teardown moved
//! to `crates/vike-datahub/src/recorder.rs`; what stays here is everything that is not a process —
//! the profile, the membership, the sessions, the runtime, the venue feeds and the watchdogs.
//!
//! ⚠ **The justification is RESPONSIBILITY, not socket economy, and nothing in this crate may be
//! written as if it were the latter.** The duplication was MEASURED and it is small: on the CI box
//! `tradehub` held bybit BTCUSDT while the recorder held polymarket + binance BTCUSDT.P — three
//! venues, overlap ZERO — and `crates/vike-model/src/venue_caps.rs` has fifteen fields and not one
//! subscription counter, so on those venues a duplicate subscription consumes no rationed resource.
//! The merge is worth doing because two crates answering one question is confusing.
//!
//! ⚠ **What the merge did NOT overturn is this file's own objection**, which was never about
//! serving: *"recording needs venue feeds, and those drag k256/keccak/tungstenite/rustls into
//! whatever links them … the server's dependency tree, audited by `cargo deny` on the box that also
//! signs real orders, stays small."* That argument is against feeds sharing a process with the
//! BACKTEST SERVER, and ruling 7 takes compute out of the data daemon entirely — the `Run*` verbs go
//! to `vike-backend backtest --addr`. What is left in the data daemon is feeds + store + serving,
//! which the objection never reached. The dependency weight is still gated the same way it was: the
//! venue features below are OPTIONAL, and `vike-datahub` forwards them under `record-polymarket` /
//! `record-binance`, so a data server built without them links no bridge at all.
//!
//! **Why not `vike-app`**: a tape that only exists while a desktop app is open has a hole wherever
//! the customer closed it. **Why not `vike-tradehub`**: recording must run whether or not you are
//! trading, and a trading daemon restart must not punch a hole in the tape. Neither of those moved.
//!
//! ## Scope of this crate today
//! The subscription model ([`config`]), the live family membership that becomes a
//! [`vike_data::GroupResolver`] ([`membership`]), the subscription driver that keeps a venue feed
//! pointed at exactly that membership ([`session`]), and the daemon tick that ties the two together
//! ([`runtime`]) — all four pure and testable without a network, because [`session`] drives the
//! [`vike_data::live::DataClient`] TRAIT and [`runtime`] drives a [`runtime::VenueFeed`], so the
//! concrete venue is the MOUNT's choice and this library never depends on a bridge crate. Gap
//! surfacing and the backfill paths are the following steps; see the spec's sequencing.
//!
//! ## The silence watchdog is a PAIR of modules
//! [`liveness`] decides WHICH subscribed series stopped receiving rows; [`alerts`] decides who is
//! TOLD. They were one and a half for a while — the verdict was computed correctly and then
//! discarded into two `tracing::warn!`s — which is why they are two modules now: detection and
//! delivery fail independently, and only one of them had ever been built.

// Where a silent series GOES: the `vike-alerting` mount that turns [`liveness`]' verdict into a
// delivered alert. Its own module because detection and notification are separately wrong-able —
// this one was missing entirely while `liveness` had been right all along.
pub mod alerts;
pub mod config;
// The silence watchdog — which subscribed series stopped receiving rows. Its own module because
// "subscribed and connected but silent" is a distinct failure from anything `session` or the loss
// counters can see; see its doc.
pub mod liveness;
pub mod membership;
pub mod runtime;
pub mod session;
pub mod venues;

pub use alerts::RecorderAlerts;
pub use config::{Alerting, Backfill, Maintenance, ProfileError, RecorderProfile, Subscription};
pub use membership::Membership;
pub use runtime::{FeedTick, RecorderRuntime, VenueFeed};
pub use session::{ReconcileReport, Stream, SubscriptionSet};
