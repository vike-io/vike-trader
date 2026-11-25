//! `vike_utility::word_enum!`'s contract, held on every enum this crate declares through it (both
//! forms), with each enum's words pinned byte for byte and, where the enum carries serde, its serde
//! spelling held equal to its word.

use crate::run_request::decide::DecideMode;
use crate::run_request::keep_trials::KeepTrials;
use crate::run_request::replay::ReplayKind;
use crate::{
    ContractCfdUnderlying, JournalArchiveFull, MountLossAction, NotionalKind,
    ReconcileDivergenceKind, ReconcilePolicyName, StoreKindId, StreamLabel, TradingState,
    VenueNetwork,
};

/// EXHAUSTIVENESS and the pinned bytes: `expected` is a `match` with no `_` arm, so a variant added
/// to the enum does not compile here until its word is pinned, and `ALL` must be exactly the listed
/// variants in this order (an array of another length does not compile either).
macro_rules! assert_exhaustive_words {
    ($kind:ident { $($variant:ident => $word:literal,)+ }) => {{
        fn expected(member: $kind) -> &'static str {
            match member {
                $($kind::$variant => $word,)+
            }
        }
        assert_eq!($kind::ALL, [$($kind::$variant,)+], "ALL is every variant, in declaration order");
        assert_eq!($kind::WORDS, [$($word,)+], "WORDS is ALL's words, in ALL's order");
        for member in $kind::ALL {
            assert_eq!(member.word(), expected(member), "{member:?}");
        }
    }};
}

/// The members' contract: every word distinct (also ignoring case, or `parse` would be ambiguous),
/// `from_word` the EXACT inverse of `word`, `parse` the case-INSENSITIVE one, and both refuse a
/// word no member has.
macro_rules! assert_word_contract {
    ($kind:ident) => {{
        let mut lowered: Vec<String> =
            $kind::WORDS.iter().map(|w| w.to_ascii_lowercase()).collect();
        lowered.sort_unstable();
        lowered.dedup();
        assert_eq!(lowered.len(), $kind::WORDS.len(), "two words differ only by case");
        for member in $kind::ALL {
            let word = member.word();
            assert_eq!($kind::from_word(word), Some(member), "{word}");
            assert_eq!($kind::parse(word), Some(member), "{word}");
            let upper = word.to_ascii_uppercase();
            let lower = word.to_ascii_lowercase();
            assert_eq!($kind::parse(&upper), Some(member), "parse is case-insensitive: {upper}");
            assert_eq!($kind::parse(&lower), Some(member), "parse is case-insensitive: {lower}");
            if upper != word {
                assert_eq!($kind::from_word(&upper), None, "from_word is exact: {upper}");
            }
            assert_eq!($kind::parse(&format!(" {word}")), None, "nothing here trims");
        }
        assert_eq!($kind::from_word("no-such-word"), None);
        assert_eq!($kind::parse("no-such-word"), None);
        assert_eq!($kind::parse(""), None);
    }};
}

/// The serde tag (or alias) READS as the word: a stored or wire word and the macro's word are one.
macro_rules! assert_serde_reads_the_word {
    ($kind:ident) => {{
        for member in $kind::ALL {
            let read: $kind = serde_json::from_str(&format!("\"{}\"", member.word()))
                .unwrap_or_else(|e| panic!("{member:?}'s word is not its serde spelling: {e}"));
            assert_eq!(read, member);
        }
    }};
}

/// The serde tag WRITES as the word, byte for byte.
macro_rules! assert_serde_writes_the_word {
    ($kind:ident) => {{
        for member in $kind::ALL {
            let written = serde_json::to_string(&member).expect("a unit variant serializes");
            assert_eq!(written, format!("\"{}\"", member.word()), "{member:?}");
        }
    }};
}

#[test]
fn venue_network_words() {
    assert_exhaustive_words!(VenueNetwork { Mainnet => "mainnet", Testnet => "testnet", });
    assert_word_contract!(VenueNetwork);
    // The wire's reader is the EXACT door: `TESTNET` stays refused.
    assert_eq!(VenueNetwork::from_word("TESTNET"), None);
    assert_eq!(VenueNetwork::from_word("testnet"), Some(VenueNetwork::Testnet));
}

/// The journal and the wire keep the variant name (`trading_state_tests.rs` pins that); the word is
/// the lowercase alias a run profile reads, and an operator may type it in any case.
#[test]
fn trading_state_words() {
    assert_exhaustive_words!(TradingState {
        Active => "active",
        Reducing => "reducing",
        Halted => "halted",
    });
    assert_word_contract!(TradingState);
    assert_serde_reads_the_word!(TradingState);
    assert_eq!(TradingState::parse("HALTED"), Some(TradingState::Halted));
}

#[test]
fn notional_kind_words() {
    assert_exhaustive_words!(NotionalKind { Linear => "linear", Inverse => "inverse", });
    assert_word_contract!(NotionalKind);
    assert_serde_reads_the_word!(NotionalKind);
    assert_serde_writes_the_word!(NotionalKind);
}

#[test]
fn contract_cfd_underlying_words() {
    assert_exhaustive_words!(ContractCfdUnderlying {
        Index => "index",
        Metal => "metal",
        Commodity => "commodity",
        Share => "share",
        Crypto => "crypto",
        Bond => "bond",
        Fx => "fx",
        Other => "other",
    });
    assert_word_contract!(ContractCfdUnderlying);
    assert_serde_reads_the_word!(ContractCfdUnderlying);
    assert_serde_writes_the_word!(ContractCfdUnderlying);
}

/// `ReplayKind` derives `Deserialize` only (a profile reads it; nothing writes it).
#[test]
fn replay_kind_words() {
    assert_exhaustive_words!(ReplayKind { Bar => "bar", Tick => "tick", });
    assert_word_contract!(ReplayKind);
    assert_serde_reads_the_word!(ReplayKind);
}

#[test]
fn decide_mode_words() {
    assert_exhaustive_words!(DecideMode {
        Sequential => "sequential",
        Simultaneous => "simultaneous",
    });
    assert_word_contract!(DecideMode);
}

/// `series` is no member: the engine refuses it BY NAME, so the roster must not admit it.
#[test]
fn keep_trials_words() {
    assert_exhaustive_words!(KeepTrials {
        None => "none",
        Scalars => "scalars",
        Returns => "returns",
    });
    assert_word_contract!(KeepTrials);
    assert_eq!(KeepTrials::parse("series"), None);
    assert_eq!(
        crate::run_request::flag_vocab::value_roster("--keep-trials"),
        KeepTrials::WORDS,
        "the flag row renders the type's words"
    );
}

/// Stored rows keep their bytes: `flatten` / `cancel_only`, `flatten` the one default.
#[test]
fn mount_loss_action_words() {
    assert_exhaustive_words!(MountLossAction {
        Flatten => "flatten",
        CancelOnly => "cancel_only",
    });
    assert_word_contract!(MountLossAction);
    assert_serde_reads_the_word!(MountLossAction);
    assert_serde_writes_the_word!(MountLossAction);
    assert_eq!(MountLossAction::default(), MountLossAction::Flatten);
}

/// The `config.journal_archive_full` row's two words: `keep`, the default, deletes nothing; `drop_oldest` is opt-in.
#[test]
fn journal_archive_full_words() {
    assert_exhaustive_words!(JournalArchiveFull {
        Keep => "keep",
        DropOldest => "drop_oldest",
    });
    assert_word_contract!(JournalArchiveFull);
    assert_serde_reads_the_word!(JournalArchiveFull);
    assert_serde_writes_the_word!(JournalArchiveFull);
    assert_eq!(JournalArchiveFull::default(), JournalArchiveFull::Keep);
}

/// The word is the variant name: what the observe wire's held alert, an alert rule and the
/// journaled policy map carry.
#[test]
fn reconcile_divergence_kind_words() {
    assert_exhaustive_words!(ReconcileDivergenceKind {
        MissingFill => "MissingFill",
        MissingTerminal => "MissingTerminal",
        OrphanLocalOrder => "OrphanLocalOrder",
        UnknownOrder => "UnknownOrder",
        PositionDrift => "PositionDrift",
        PositionOnlyExternal => "PositionOnlyExternal",
        OrphanLocalPosition => "OrphanLocalPosition",
        BalanceDrift => "BalanceDrift",
        JournalDivergence => "JournalDivergence",
        OrderTermsDrift => "OrderTermsDrift",
    });
    assert_word_contract!(ReconcileDivergenceKind);
    assert_serde_reads_the_word!(ReconcileDivergenceKind);
    assert_serde_writes_the_word!(ReconcileDivergenceKind);
    for kind in ReconcileDivergenceKind::ALL {
        assert_eq!(format!("{kind:?}"), kind.word(), "the Debug text the wire used is the word");
    }
}

/// The ORDER is published (`rosters.json`): the default first, the two absolutes, the refinement.
#[test]
fn reconcile_policy_name_words() {
    assert_exhaustive_words!(ReconcilePolicyName {
        Hybrid => "hybrid",
        Synthesize => "synthesize",
        Quarantine => "quarantine",
        ExternalQuarantine => "external-quarantine",
    });
    assert_word_contract!(ReconcilePolicyName);
    assert_serde_reads_the_word!(ReconcilePolicyName);
    assert_serde_writes_the_word!(ReconcilePolicyName);
}

#[test]
fn store_kind_id_words() {
    assert_exhaustive_words!(StoreKindId {
        Bar => "bar",
        Book => "book",
        Chain => "chain",
        Cohort => "cohort",
        Depth => "depth",
        Equity => "equity",
        ExecFill => "exec_fill",
        ExecForecast => "exec_forecast",
        ExecFunding => "exec_funding",
        ExecOrder => "exec_order",
        FinancialFact => "financial_fact",
        InstitutionalHolding => "institutional_holding",
        OptionTrade => "option_trade",
        PerpMetrics => "perp_metrics",
        Properties => "properties",
        Quote => "quote",
        Text => "text",
        TextScore => "text_score",
        Trade => "trade",
    });
    assert_word_contract!(StoreKindId);
    assert_serde_reads_the_word!(StoreKindId);
    assert_serde_writes_the_word!(StoreKindId);
    // A store kind is read EXACTLY: the strict door refuses any other case.
    assert_eq!(StoreKindId::from_word("BAR"), None);
    assert!("Bar".parse::<StoreKindId>().is_err());
}

/// No serde assertion on purpose: `StreamLabel`'s serde TAG is the variant name, not the word
/// (`crates/vike-model/src/stream/label_tests.rs`'s `the_serde_tags_are_the_variant_names` holds it).
#[test]
fn stream_label_words() {
    assert_exhaustive_words!(StreamLabel {
        Quotes => "quotes",
        Trades => "trades",
        Book => "book",
        Depth => "depth",
    });
    assert_word_contract!(StreamLabel);
}
