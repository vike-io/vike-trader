//! The account itself: who the key trades, its modes and leverage, its link and quote rate.

// The one-time deposit-wallet approvals, sent through the relayer client (`crate::settlement::redeem_relayer`).
pub(crate) mod onboarding;
// The pre-order READ of the wallet's balance and allowance: refuse in words what is missing.
pub(crate) mod preflight;
