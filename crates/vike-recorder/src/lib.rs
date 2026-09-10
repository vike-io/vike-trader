//! `vike-recorder` — the HEADLESS live-tape recorder.
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
//! | TWO daemons, not one process (§8.1) | this is its OWN binary; `vike-datahub` reads the same store root from a separate process |
//! | families are GENERAL across venues (§8.2) | [`config::Subscription::family`] — one key, Polymarket rotates its membership, a static venue filters |
//! | backfill is PER-SUBSCRIPTION (§8.3) | [`config::Backfill`] on the subscription |
//! | two-tier write path ADOPTED (§8.4) | not yet — sequenced after this skeleton, deliberately built against a real writer |
//!
//! **Why a separate binary rather than a mode of `vike-datahub`**: recording needs venue feeds, and
//! those drag k256/keccak/tungstenite/rustls into whatever links them. Keeping them out of the
//! backtest server means a panic in feed-parsing cannot take serving down, and the server's
//! dependency tree — audited by `cargo deny` on the box that also signs real orders — stays small.
//! The accepted cost is that datahub cannot report recorder lag; if that ever matters the fix is an
//! IPC surface between the two, NOT co-hosting, because the dependency argument does not weaken.
//!
//! **Why not `vike-app`**: a tape that only exists while a desktop app is open has a hole wherever
//! the customer closed it. **Why not `vike-tradehub`**: recording must run whether or not you are
//! trading, and a trading daemon restart must not punch a hole in the tape.
//!
//! ## Scope of this crate today
//! The subscription model ([`config`]), the live family membership that becomes a
//! [`vike_data::GroupResolver`] ([`membership`]), the subscription driver that keeps a venue feed
//! pointed at exactly that membership ([`session`]), and the daemon tick that ties the two together
//! ([`runtime`]) — all four pure and testable without a network, because [`session`] drives the
//! [`vike_data::live::DataClient`] TRAIT and [`runtime`] drives a [`runtime::VenueFeed`], so the
//! concrete venue is the BINARY's choice and this library never depends on a bridge crate. Gap
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
// The recorder CLI as a library function, so the bin and the `vike` multicall dispatcher reach one
// copy. Not feature-gated: this crate's venue feeds are, but the command line itself is not.
pub mod recorder_cli;

pub use alerts::RecorderAlerts;
pub use config::{Alerting, Backfill, Maintenance, ProfileError, RecorderProfile, Subscription};
pub use membership::Membership;
pub use runtime::{FeedTick, RecorderRuntime, VenueFeed};
pub use session::{ReconcileReport, Stream, SubscriptionSet};
