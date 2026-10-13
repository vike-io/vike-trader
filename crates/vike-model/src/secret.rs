//! `secret` — [`Secret<T>`], a value that never prints.
//!
//! # The contract
//!
//! A `Secret<T>` holds a credential VALUE (an API secret, a private key, an HMAC key) and makes the
//! one thing that leaks values by accident — formatting — impossible to do by accident:
//!
//! - `Debug` prints `Secret(<redacted>)` and nothing of the value, so a derived `Debug` on a struct
//!   that carries one, a `{:?}` in a log line or a panic message, and an `Option<Secret<_>>` all
//!   stay redacted without anybody remembering to hand-write anything;
//! - there is NO `Display` and NO `Serialize`, so the value cannot reach a log line, a UI string or
//!   a wire document except through [`Secret::expose`] / [`Secret::into_inner`], which are the two
//!   names a reviewer greps for.
//!
//! `vike_secrets::credentials::Credentials` carries its three values as `Secret<String>`, read
//! through [`Secret::expose`] at each signer and header. The redacting `Debug`s written by hand so
//! far (`Credentials`' — a test pins its four-character key tail — `SecretMap`'s, `NodeKeys`') stay
//! as they are; this is the shared shape new credential-carrying types take, so the next one is not
//! a fourth hand-written `Debug`.
//!
//! # Equality is by VALUE and is NOT constant-time
//!
//! `PartialEq`/`Eq` compare the wrapped values with `T`'s own `==`, which short-circuits on the first
//! differing byte. That is fine for what this type is for (keeping a value out of formatted output)
//! and wrong for verifying a presented secret against a stored one over a network, where the timing
//! of a mismatch is an oracle. A MAC or token check must use a constant-time comparison of its own
//! (the node-key handshake verifies an HMAC, never a raw `==`); nothing here provides one.
//!
//! # No secret value enters this crate
//!
//! This module defines the TYPE and nothing else: no function here reads, stores or loads a value,
//! so `vike-model` keeps the rule `vike-venues`' `credential_keys` module states — names only, never a value.

/// A value that never prints. See the module doc for the contract.
///
/// ```
/// use vike_model::secret::Secret;
/// let key = Secret::new(String::from("hunter2"));
/// assert_eq!(format!("{key:?}"), "Secret(<redacted>)");
/// assert_eq!(key.expose(), "hunter2");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wrap `value`.
    #[must_use]
    pub fn new(value: T) -> Self {
        Secret(value)
    }

    /// The value, borrowed — the ONE read door. Every call site is a place the value is handed to
    /// something that will use it (a signer, a request header), and is named so a review can find it.
    #[must_use]
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// The value, by move — [`Secret::expose`] for a consumer that needs ownership.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> From<T> for Secret<T> {
    fn from(value: T) -> Self {
        Secret(value)
    }
}

impl<T> std::fmt::Debug for Secret<T> {
    /// `Secret(<redacted>)`, whatever `T` is — the bound is deliberately absent, so even a `T` with
    /// no `Debug` of its own can sit inside a struct that derives one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
#[path = "secret_tests.rs"]
mod tests;
