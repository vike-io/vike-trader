//! The two IBKR `ReconClient`s, one per backend, under one contract: each resolves the symbol's
//! conId at connect and filters every account-wide report to it (per symbol, decision 0140).
//! `cpapi` is the REST client for a cpapi exec (feature `ibkr-cpapi`); `socket` is the ibapi client
//! on its own API client id (feature `ibkr`, because it reuses the IB-status and order-type
//! vocabulary of `cpapi`).

#[cfg(feature = "ibkr-cpapi")]
pub mod cpapi;
#[cfg(feature = "ibkr")]
pub mod socket;
#[cfg(feature = "ibkr")]
pub(crate) mod summary;
