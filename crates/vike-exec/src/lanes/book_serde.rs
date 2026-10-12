//! `BookUpdate`'s serde pair: an `Arc<L2Book>` on the wire as a bare `L2Book`.

use std::sync::Arc;
use vike_model::L2Book;

/// Serialize `Arc<L2Book>` exactly as a bare `L2Book` — see [`super::BookUpdate`]'s serde note.
/// Hand-written rather than turning on serde's `rc` feature, which would reach EVERY derive in the
/// tree rather than this one pinned site.
pub(super) fn ser_book<S: serde::Serializer>(book: &Arc<L2Book>, s: S) -> Result<S::Ok, S::Error> {
    serde::Serialize::serialize(&**book, s)
}

/// The `ser_book` inverse: read a bare `L2Book` and wrap it in a FRESH `Arc` (the book is value
/// state on this lane, never an identity).
pub(super) fn de_book<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Arc<L2Book>, D::Error> {
    serde::Deserialize::deserialize(d).map(Arc::new)
}
