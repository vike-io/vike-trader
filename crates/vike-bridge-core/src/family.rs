//! **The rolling-family lane's ONE contract: the venue's half.** The gatewayhub answers `ResolveFamily`
//! (the symbols a rotating family should be streaming now) through ONE engine,
//! `crates/vike-gatewayhub/src/rest/family.rs`'s `FamilyHub`, and the venue's whole difference is a
//! [`FamilySpec`] in that venue's bridge: the grammar a family name must have, and the resolver that turns
//! one into the live symbols. The engine + spec split of `crate::credentialed_klines`, for the family lane.
//!
//! # What the engine does, so a spec does not
//!
//! It runs [`FamilySpec::admit`] FIRST, before a resolver exists and before any venue is asked; keeps one
//! resolver per (connection, family), built by [`FamilySpec::resolver`] on the family's first ask and
//! dropped with the connection; bounds how many a connection may hold; and hands back the resolver's
//! answer, `Err` included.
//!
//! # What a spec promises
//!
//! * **`admit` is offline**: no socket, no read, no state; its refusal is in words the peer reads.
//! * **A resolver's failure is `Err`, never an empty set** (an empty set unsubscribes every live stream),
//!   and a failed pass leaves the resolver as it was, so the next pass recovers without a rebuild.

use vike_data::live::SymbolResolver;

/// **A venue's rolling-family lane**, as the gatewayhub's engine asks it. See the module doc.
pub trait FamilySpec: Send + Sync {
    /// The venue whose families this lane resolves: a `vike_model::VENUES` slug.
    fn venue(&self) -> &'static str;

    /// Refuse a name that is no family of this venue, offline, in words.
    fn admit(&self, family: &str) -> Result<(), String>;

    /// The resolver of a family that passed [`FamilySpec::admit`]: stateful (a window cache, a clock),
    /// kept by the engine for the connection's life.
    fn resolver(&self, family: &str) -> Result<Box<dyn SymbolResolver>, String>;
}
