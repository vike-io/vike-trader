//! The sidecar's `ready` balance as ONE wallet frame (`Event::AccountState`) the engine adopts.
//!
//! # Why a frame, and why only at `ready`
//!
//! A Dukascopy engine has NO reconcile leg (decision 0168: `crate::adapter` hands the node none), so
//! nothing ever told it the venue's balance: it ran in `BalanceMode::Delta` on the mount's seed cash
//! (1000 on the owner's demo box, while the sidecar said `ready balance=10000.0`) and judged every
//! order against that, denying a 1000-unit foreign-pair order as `insufficient-margin`. The frame is
//! the other door a venue balance can use (`crates/vike-exec/src/account.rs`'s `apply_account_state`):
//! it writes the engine's balance absolutely and flips the account to `BalanceMode::Authoritative`.
//! The seed is NOT touched, so the drawdown latch's capital base (a sum of seeds) is what it was.
//!
//! # ⚠ What the figure is, and the residual that comes with it
//!
//! `ready.balance` is `IAccount.getBalance()`, which the vendor's Javadoc calls the balance of the
//! PREVIOUS END-OF-DAY processing: its freshness is only "at start". In `Authoritative` mode equity is
//! `balance + unrealized` with NO realized-PnL term (`crates/vike-exec/src/account/valuation.rs`'s
//! `equity_all`), and nothing here refreshes the balance after the one frame. So:
//!
//! * realized PnL of the engine's own trades stays OUT of equity until the next start of the sidecar;
//! * a mid-day restart adopts the vendor's previous end-of-day figure, not the live one.
//!
//! Accepted by the owner for a venue that arms at `demo` only (the venue enforces its own margin at
//! the fill, and the drawdown latch reads own PnL). The cure is a live figure from the sidecar
//! (`IAccount.getBaseEquity()` on an `account` push): a Java change and a jar release, not this file.
//!
//! # The gate: dollars on a dollar ledger, or nothing
//!
//! A frame is built ONLY when the currency the sidecar STATED for the account is `USD` AND the money
//! the engine's mount counts in is `USD` (the mount's quote currency). Every other case sends nothing
//! and the engine stays on its seed cash. The frame path carries events, not the reconcile leg, so the
//! node's balance gate (`crates/vike-tradehub/src/remote_backend/balance_gate.rs`'s `BalanceGate`)
//! never sees it: this producer carries the equivalent rule itself, the narrow one that agrees with
//! every binding the node can give the account. A balance in another money would otherwise be adopted
//! as dollars on a plain ledger (a lone balance is taken whatever its asset is called), and a USD
//! account on a `USDJPY` mount keeps its numbers in yen. The mount's money is read the way
//! `crates/vike-gatewayhub/src/trade/adapters/dukascopy_rate.rs`'s `ledger_for` reads it; an unstated
//! currency is unknown, never USD (`crate::quotes::QuoteBus::account_currency`).
//!
//! What the operator sees when it is withheld: one `warn` line in the gatewayhub's journal naming the
//! reason ([`wallet_frame`] says it on the reader's thread), and an engine that stays on its seed. There
//! is no node-side hold or reason for this venue, because there is no leg to hold.
//!
//! # One frame per sidecar start
//!
//! The reader calls [`wallet_frame`] from its first-`ready` arm only, so a start is ONE frame and a
//! restart of the sidecar re-adopts. The frame carries no account (`route_key: None`): the mount's
//! sender stamps the account's key (`crate::exec`'s `pump`).

use std::fmt;

use vike_model::events::{AccountState, Event};

use crate::recon_client::VENUE;

/// Shared tracing target for this module's diagnostics.
const TARGET: &str = "vike_dukascopy::ready_balance";

/// The only money a frame is built in, and the only account and mount money it is built for.
const DOLLAR: &str = "USD";

/// Why [`wallet_frame`] built nothing. The reader prints it; nothing else branches on it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Withheld {
    /// The sidecar stated no account currency: a jar from before the field, or a word that is no
    /// three-letter code. Unknown is never read as USD.
    CurrencyNotStated,
    /// The sidecar stated this money, and it is not USD.
    CurrencyNotUsd(String),
    /// The engine is mounted on a symbol that is not a six-letter pair quoted in USD.
    MountNotUsd { mount: String },
    /// Zero, negative or non-finite: adopting it would deny every opening order.
    BalanceNotUsable(f64),
}

impl fmt::Display for Withheld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrencyNotStated => write!(
                f,
                "the sidecar states no account currency (a jar from before the field), so its \
                 balance is not known to be dollars"
            ),
            Self::CurrencyNotUsd(stated) => write!(
                f,
                "the account's money is {stated}, and a balance is adopted in {DOLLAR} only"
            ),
            Self::MountNotUsd { mount } => write!(
                f,
                "the engine is mounted on `{mount}`, which is not a pair quoted in {DOLLAR}, so \
                 its ledger is not dollars"
            ),
            Self::BalanceNotUsable(balance) => {
                write!(f, "the sidecar's balance ({balance}) is not a positive number")
            }
        }
    }
}

/// Whether `mount` is a six-letter pair quoted in USD (`EURUSD`, `XAUUSD`), however it is spelled.
fn mount_is_quoted_in_dollars(mount: &str) -> bool {
    let pair = mount.trim().to_ascii_uppercase();
    pair.len() == 6 && pair.bytes().all(|b| b.is_ascii_uppercase()) && pair[3..] == *DOLLAR
}

/// The wallet frame for the sidecar's `ready` balance, or the reason there is none.
///
/// `stated` is the account currency the sidecar stated, as the price lane's bus keeps it (upper-case
/// three letters, `None` = not stated); `mount` is the symbol the engine was started on; `ts` is the
/// frame's time in epoch ms.
///
/// It SAYS what it decided, once, on the caller's thread: an `info` line when it builds the frame, a
/// `warn` line naming the reason when it does not (the operator's only sign that the engine is still
/// on its seed).
///
/// # Errors
/// [`Withheld`], in the order of the checks: the stated money, the mount's money, the figure.
pub(crate) fn wallet_frame(
    balance: f64,
    stated: Option<&str>,
    mount: &str,
    ts: i64,
) -> Result<Event, Withheld> {
    if let Err(why) = gate(balance, stated, mount) {
        tracing::warn!(
            target: TARGET, %mount,
            "the sidecar's ready balance was NOT sent to the engine: {why}; the engine stays on its \
             seed cash"
        );
        return Err(why);
    }
    tracing::info!(
        target: TARGET, balance, %mount,
        "the sidecar's ready balance is the engine's balance (the vendor's previous end-of-day \
         figure, fresh only at start)"
    );
    Ok(Event::AccountState(AccountState {
        venue: VENUE.into(),
        balances: vec![(DOLLAR.to_string(), balance)],
        ts,
        route_key: None,
    }))
}

/// The gate alone, in the order its reasons are given.
fn gate(balance: f64, stated: Option<&str>, mount: &str) -> Result<(), Withheld> {
    match stated {
        None => return Err(Withheld::CurrencyNotStated),
        Some(DOLLAR) => {}
        Some(other) => return Err(Withheld::CurrencyNotUsd(other.to_string())),
    }
    if !mount_is_quoted_in_dollars(mount) {
        return Err(Withheld::MountNotUsd { mount: mount.to_string() });
    }
    if !(balance.is_finite() && balance > 0.0) {
        return Err(Withheld::BalanceNotUsable(balance));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: i64 = 1_760_000_000_000;

    fn frame(balance: f64, stated: Option<&str>, mount: &str) -> Result<Event, Withheld> {
        wallet_frame(balance, stated, mount, TS)
    }

    /// The measured case (2026-10-20, account 8): a USD account, a `EURUSD` mount, a ready balance of
    /// 10000.0. ONE frame, the venue's id, dollars, the figure, the caller's time, and no account key
    /// (the mount's sender stamps it).
    #[test]
    fn a_usd_account_on_a_usd_mount_is_one_frame_in_dollars() {
        let event = frame(10_000.0, Some("USD"), "EURUSD").expect("a frame");
        assert_eq!(
            event,
            Event::AccountState(AccountState {
                venue: "dukascopy".into(),
                balances: vec![("USD".to_string(), 10_000.0)],
                ts: TS,
                route_key: None,
            })
        );
    }

    /// The mount's spelling is the caller's (a labelled account may choose it): case and padding do not
    /// matter, a six-letter pair is what counts, and the money is its last three letters.
    #[test]
    fn the_mount_is_read_by_its_last_three_letters_however_it_is_spelled() {
        for mount in ["EURUSD", "eurusd", "  GbpUsd ", "XAUUSD"] {
            assert!(frame(1.0, Some("USD"), mount).is_ok(), "{mount:?}");
        }
        for mount in ["USDJPY", "EURGBP", "EURUSDX", "EUR/USD", "USD", ""] {
            assert_eq!(
                frame(1.0, Some("USD"), mount),
                Err(Withheld::MountNotUsd { mount: mount.to_string() }),
                "{mount:?}"
            );
        }
    }

    /// Another money, or none: nothing is built. A JPY account on a `USDJPY` mount has a ledger in its
    /// own money, and still is not adopted: only dollars are.
    #[test]
    fn only_a_stated_usd_account_is_adopted() {
        assert_eq!(frame(1.0, Some("EUR"), "EURUSD"), Err(Withheld::CurrencyNotUsd("EUR".into())));
        assert_eq!(frame(1.0, Some("JPY"), "USDJPY"), Err(Withheld::CurrencyNotUsd("JPY".into())));
        assert_eq!(frame(1.0, None, "EURUSD"), Err(Withheld::CurrencyNotStated));
    }

    /// A figure that would deny every order (or poison the fold) is not adopted: zero, negative, NaN, inf.
    #[test]
    fn an_unusable_balance_is_not_adopted() {
        for balance in [0.0, -5.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let got = frame(balance, Some("USD"), "EURUSD");
            assert!(matches!(got, Err(Withheld::BalanceNotUsable(_))), "{balance}: {got:?}");
        }
    }

    /// WHAT THE OPERATOR SEES: a frame that is built is one `info` line; a frame that is not is one `warn` line
    /// naming the reason and saying the engine stays on its seed. Said on the caller's thread, so a test sees it.
    #[test]
    fn the_operator_gets_one_line_saying_what_was_decided() {
        let (out, lines) = vike_log::capture::captured(|| frame(10_000.0, Some("USD"), "EURUSD"));
        assert!(out.is_ok());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].level, tracing::Level::INFO);
        assert!(lines[0].message.contains("is the engine's balance"), "{lines:?}");

        for (stated, mount, balance, reason) in [
            (Some("EUR"), "EURUSD", 1.0, "money is EUR"),
            (None, "EURUSD", 1.0, "no account currency"),
            (Some("USD"), "USDJPY", 1.0, "`USDJPY`"),
            (Some("USD"), "EURUSD", 0.0, "not a positive number"),
        ] {
            let (out, lines) = vike_log::capture::captured(|| frame(balance, stated, mount));
            assert!(out.is_err());
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert_eq!(lines[0].level, tracing::Level::WARN, "{lines:?}");
            let said = &lines[0].message;
            assert!(said.contains("NOT sent to the engine"), "{said}");
            assert!(said.contains(reason), "{reason}: {said}");
            assert!(said.contains("seed cash"), "{said}");
        }
    }

    /// The reason is words an operator can act on: it names the money, the mount or the figure.
    #[test]
    fn each_reason_names_what_it_is_about() {
        assert!(Withheld::CurrencyNotStated.to_string().contains("no account currency"));
        assert!(Withheld::CurrencyNotUsd("EUR".into()).to_string().contains("EUR"));
        assert!(Withheld::MountNotUsd { mount: "USDJPY".into() }.to_string().contains("USDJPY"));
        assert!(Withheld::BalanceNotUsable(0.0).to_string().contains("not a positive number"));
    }
}
