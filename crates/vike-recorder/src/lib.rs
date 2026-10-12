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
//! # ⚠ THE RECORDER IS NOT ITS OWN PROCESS ANY MORE
//!
//! Ruling 10 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` merged the
//! recorder daemon into the data server: the mount, the tick cadence and the whole teardown are
//! `crates/vike-datahub/src/recorder.rs`, and what stays here is everything that is not a process.
//! The venue TABLE left too, on decision 0092 (one venue client per process): it is
//! `crates/vike-datahub/src/recording.rs`, and every recorder feed takes its client from that
//! daemon's feed broker. This crate names no bridge and declares no venue feature, so a data server
//! built without `record-<venue>` links no bridge for recording. The justification for the merge is
//! RESPONSIBILITY (two crates answering one question was confusing), not socket economy: the
//! measured duplication was small. The rest of the argument is on this crate's `CLAUDE.md`.
//!
//! ## Scope of this crate today
//! The subscription model ([`config`]), the live family membership that becomes a
//! [`vike_data::GroupResolver`] ([`membership`]), the subscription driver that keeps a venue feed
//! pointed at exactly that membership ([`session`]), and the daemon tick that ties the two together
//! ([`runtime`]) — all four pure and testable without a network, because [`session`] drives the
//! [`vike_data::DataClient`] TRAIT and [`runtime`] drives a [`runtime::VenueFeed`], so the
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
// The ONE `VenueFeed` implementation: a venue label, a resolver and a client, put together by
// whoever constructs it.
pub mod assembled;
pub mod config;
// The silence watchdog — which subscribed series stopped receiving rows. Its own module because
// "subscribed and connected but silent" is a distinct failure from anything `session` or the loss
// counters can see; see its doc.
pub mod liveness;
pub mod membership;
// The venue-free family resolvers — a glob over any venue's own instrument list.
pub mod resolve;
pub mod runtime;
pub mod session;
