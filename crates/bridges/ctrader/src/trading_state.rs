//! Whether the BROKER lets this account trade a symbol, read off facts the handshake already
//! fetches, and the pure rule that turns it into a refusal in words.
//!
//! Two wire facts, both decoded all along and ignored until this module:
//!
//! - `ProtoOASymbol.tradingMode` (`ProtoOATradingMode`: `ENABLED`, `DISABLED_WITHOUT_PENDINGS_EXECUTION`,
//!   `DISABLED_WITH_PENDINGS_EXECUTION`, `CLOSE_ONLY_MODE`; an absent field is the proto default
//!   `ENABLED`). It rides the full symbol of the handshake's one `SymbolById` batch, so every listed
//!   symbol carries it. (`NO_TRADING` and `CLOSE_ONLY` are `ProtoOAAccessRights` of the TRADER, an
//!   account-wide fact this module does not read; the symbol's own words are the four above.)
//! - `ProtoOALightSymbol.enabled` ("TRUE if the symbol is visible for traders"); `Some(false)` is a
//!   symbol the broker lists and hides.
//!
//! Without them a listed-but-disabled symbol was admitted and then died at the broker as its own bare
//! reject. [`refusal`] says WHY instead, and refuses only an order that OPENS risk: a close is never
//! blocked here (a position must always be closable, and the broker decides whether it will take it).
//!
//! ⚠ **Deliberately NOT read: `schedule`, `scheduleTimeZone`, `holiday`.** They are weekly intervals in
//! a named time zone and a holiday list with recurrence and partial days: deciding "closed right now"
//! from them is a calendar computation this bridge would have to get right on every DST edge, and the
//! proto states no "closed now" flag. A closed market is therefore NOT refused here; the broker's own
//! `MARKET_CLOSED` / `SYMBOL_HAS_HOLIDAY` answer reaches the engine as the terminal `OrderRejected`
//! `<CODE>: <description>`, as before.
//!
//! The state is the LATEST the broker told this session, and it follows the broker: the exec client's symbol map is
//! shared by `Arc` and NOT swapped on a reconnect, so its restriction table is a shared cell
//! (`crate::symbols::SymbolMap::set_restrictions`) that `crate::conn::actor`'s `actor_loop` rewrites in two places: at
//! every reconnect, from the handshake it just ran, and on a `ProtoOASymbolChangedEvent`
//! (`crate::conn`'s `refresh_symbol_state`: the light list and the changed symbols' full records are asked again and
//! [`refreshed`] merges them over the old table). A symbol the broker re-enables or restricts later is seen without
//! rebuilding the session. NOT measured live: a demo account cannot be made to change a symbol, so the event's arrival
//! (and whether the broker sends it unasked) is read from the proto and played by the scripted venue only. The
//! refusal says which state it speaks from.

use std::collections::HashMap;

use crate::proto::{ProtoOaLightSymbol, ProtoOaSymbol, ProtoOaTradingMode};

/// Why the broker does not let this account OPEN a position in a symbol. A symbol with none of these is
/// absent from [`restrictions`], so the ordinary symbol costs one empty-map lookup per order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restriction {
    /// `ProtoOALightSymbol.enabled = false`: listed, but hidden from traders.
    NotVisible,
    /// `tradingMode DISABLED_WITHOUT_PENDINGS_EXECUTION`: trading off, resting orders do not execute.
    DisabledWithoutPendings,
    /// `tradingMode DISABLED_WITH_PENDINGS_EXECUTION`: trading off, already-resting orders still execute.
    DisabledWithPendings,
    /// `tradingMode CLOSE_ONLY_MODE`: only closing trades.
    CloseOnly,
}

impl Restriction {
    /// The restriction a `tradingMode` wire value names, or `None` for `ENABLED` and for a value this
    /// build does not know (an unknown word is never read as a refusal; the broker decides).
    fn from_mode(mode: i32) -> Option<Self> {
        match ProtoOaTradingMode::try_from(mode).ok()? {
            ProtoOaTradingMode::Enabled => None,
            ProtoOaTradingMode::DisabledWithoutPendingsExecution => {
                Some(Self::DisabledWithoutPendings)
            }
            ProtoOaTradingMode::DisabledWithPendingsExecution => Some(Self::DisabledWithPendings),
            ProtoOaTradingMode::CloseOnlyMode => Some(Self::CloseOnly),
        }
    }

    /// The broker's own word for the state, as the proto spells it.
    pub fn wire_word(self) -> &'static str {
        match self {
            Self::NotVisible => "enabled=false",
            Self::DisabledWithoutPendings => "tradingMode DISABLED_WITHOUT_PENDINGS_EXECUTION",
            Self::DisabledWithPendings => "tradingMode DISABLED_WITH_PENDINGS_EXECUTION",
            Self::CloseOnly => "tradingMode CLOSE_ONLY_MODE",
        }
    }

    fn meaning(self) -> &'static str {
        match self {
            Self::NotVisible => "the broker lists it but hides it from traders",
            Self::DisabledWithoutPendings => {
                "the broker has switched trading in it off, and its resting orders do not execute"
            }
            Self::DisabledWithPendings => {
                "the broker has switched trading in it off; only already-resting orders still execute"
            }
            Self::CloseOnly => "the broker accepts only closing trades in it",
        }
    }
}

/// Every restricted symbol of a handshake's lists, by symbol id. `tradingMode` (the full symbol) is the
/// more specific word and wins over `enabled=false` (the light symbol) for the same id.
pub fn restrictions(
    light: &[ProtoOaLightSymbol],
    full: &[ProtoOaSymbol],
) -> HashMap<i64, Restriction> {
    let mut out = HashMap::new();
    for s in light.iter().filter(|s| s.enabled == Some(false)) {
        out.insert(s.symbol_id, Restriction::NotVisible);
    }
    for s in full {
        if let Some(r) = s.trading_mode.and_then(Restriction::from_mode) {
            out.insert(s.symbol_id, r);
        }
    }
    out
}

/// The table after the broker said the symbols `changed` were changed: every other symbol keeps its restriction,
/// and a changed one is read afresh from `light` (the whole list, asked again: `enabled` lives on it) and `full` (the
/// changed symbols' own records: `tradingMode`). A changed symbol the answers do not mention is no longer restricted
/// (an archived symbol cannot be traded in any case; the broker decides).
pub fn refreshed(
    old: &HashMap<i64, Restriction>,
    changed: &[i64],
    light: &[ProtoOaLightSymbol],
    full: &[ProtoOaSymbol],
) -> HashMap<i64, Restriction> {
    let mut out: HashMap<i64, Restriction> =
        old.iter().filter(|(id, _)| !changed.contains(id)).map(|(id, r)| (*id, *r)).collect();
    let light: Vec<ProtoOaLightSymbol> =
        light.iter().filter(|s| changed.contains(&s.symbol_id)).cloned().collect();
    let full: Vec<ProtoOaSymbol> =
        full.iter().filter(|s| changed.contains(&s.symbol_id)).cloned().collect();
    out.extend(restrictions(&light, &full));
    out
}

/// The refusal for an order in a restricted symbol, or `None` to let it through. `opens` is true only
/// for an order the caller can show OPENS or ADDS risk (not `reduce_only`, larger than the closing room
/// its own position book proves); a close always passes, whatever the state, and the broker decides.
pub fn refusal(name: &str, restriction: Restriction, opens: bool) -> Option<String> {
    opens.then(|| {
        format!(
            "ctrader: {name} is not tradable on this account: {} ({}); nothing was sent. A \
             reduce_only order, or one no larger than an open position it closes, still goes through. \
             This is the latest state the broker reported to this session (at connect, or in a symbol-changed \
             event)",
            restriction.wire_word(),
            restriction.meaning()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Restriction; 4] = [
        Restriction::NotVisible,
        Restriction::DisabledWithoutPendings,
        Restriction::DisabledWithPendings,
        Restriction::CloseOnly,
    ];

    fn lite(symbol_id: i64, enabled: Option<bool>) -> ProtoOaLightSymbol {
        ProtoOaLightSymbol { symbol_id, enabled, ..Default::default() }
    }

    fn rich(symbol_id: i64, mode: Option<i32>) -> ProtoOaSymbol {
        ProtoOaSymbol { symbol_id, trading_mode: mode, ..Default::default() }
    }

    /// The wire table: every `tradingMode` value and `enabled` state to its restriction. Absent is
    /// `ENABLED` / unknown (nothing refused); an unknown number is nothing refused either.
    #[test]
    fn the_wire_values_map_to_restrictions() {
        use ProtoOaTradingMode as M;
        let light = [
            lite(1, Some(true)),
            lite(2, Some(false)),
            lite(3, None),
            lite(4, Some(false)),
            lite(5, Some(true)),
            lite(6, None),
            lite(7, None),
            // No full entry at all: only the light symbol's word is known.
            lite(8, Some(false)),
        ];
        let full = [
            rich(1, Some(M::Enabled as i32)),
            rich(2, None),
            rich(3, Some(M::DisabledWithoutPendingsExecution as i32)),
            // `tradingMode` is the more specific word and wins over `enabled=false` for the same id.
            rich(4, Some(M::CloseOnlyMode as i32)),
            rich(5, Some(M::DisabledWithPendingsExecution as i32)),
            rich(6, None),
            rich(7, Some(99)),
        ];
        let want: HashMap<i64, Restriction> = HashMap::from([
            (2, Restriction::NotVisible),
            (3, Restriction::DisabledWithoutPendings),
            (4, Restriction::CloseOnly),
            (5, Restriction::DisabledWithPendings),
            (8, Restriction::NotVisible),
        ]);
        assert_eq!(
            restrictions(&light, &full),
            want,
            "ENABLED / absent / an unknown number / enabled=true are never refused"
        );
    }

    /// The decision table: a restricted symbol refuses exactly an order that opens, in words that name
    /// the symbol, the venue's own word for the state and that nothing was sent; a close never refuses.
    #[test]
    fn a_restricted_symbol_refuses_only_an_opening_order_and_says_why() {
        for r in ALL {
            assert_eq!(refusal("EURUSD", r, false), None, "{r:?}: a close must always get through");
            let said =
                refusal("EURUSD", r, true).unwrap_or_else(|| panic!("{r:?}: an open passed"));
            assert!(
                said.starts_with("ctrader: EURUSD is not tradable on this account: "),
                "{said}"
            );
            assert!(
                said.contains(r.wire_word()),
                "{r:?}: the broker's own word is missing: {said}"
            );
            assert!(said.contains("nothing was sent"), "{said}");
        }
        let close_only = refusal("EURUSD", Restriction::CloseOnly, true).expect("refused");
        assert!(close_only.contains("tradingMode CLOSE_ONLY_MODE"), "{close_only}");
        let hidden = refusal("XAUUSD", Restriction::NotVisible, true).expect("refused");
        assert!(hidden.contains("XAUUSD") && hidden.contains("enabled=false"), "{hidden}");
    }

    /// A symbol-changed event re-reads only the symbols it names: a re-enabled one is freed, a newly restricted
    /// one is refused, a hidden one is refused, and every symbol it does not name keeps what it had, even
    /// when the answers happen to carry its record.
    #[test]
    fn a_changed_symbol_is_read_afresh_and_the_others_keep_their_restriction() {
        use ProtoOaTradingMode as M;
        let old: HashMap<i64, Restriction> = HashMap::from([
            (1, Restriction::CloseOnly),
            (2, Restriction::DisabledWithoutPendings),
            (3, Restriction::NotVisible),
        ]);
        let light =
            [lite(1, Some(true)), lite(2, Some(true)), lite(3, Some(false)), lite(4, Some(false))];
        let full = [
            rich(1, Some(M::Enabled as i32)),
            rich(2, Some(M::Enabled as i32)),
            rich(5, Some(M::CloseOnlyMode as i32)),
        ];
        // 1 was re-enabled; 4 became hidden; 5 became close-only; 2 and 3 were not named.
        let got = refreshed(&old, &[1, 4, 5], &light, &full);
        let want: HashMap<i64, Restriction> = HashMap::from([
            (2, Restriction::DisabledWithoutPendings),
            (3, Restriction::NotVisible),
            (4, Restriction::NotVisible),
            (5, Restriction::CloseOnly),
        ]);
        assert_eq!(got, want);
        assert_eq!(refreshed(&old, &[], &light, &full), old, "no symbol named: nothing moves");
        // A changed symbol the answers do not mention is no longer restricted.
        assert_eq!(refreshed(&old, &[2], &[], &[]).get(&2), None);
    }
}
