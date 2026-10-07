//! The SIDE map's readers: which of the market's two colours a side or a sign wears.
//!
//! Buy and sell, long and short, bid and ask, profit and loss, take-profit and stop-loss, a number above
//! zero and one below it, an Up outcome and a Down one: every screen that tells the two of a pair apart
//! deals them the market set's two colours, `up` and `down` as a graphic (a fill, a line, a marker, a bar) and
//! `up_text` and `down_text` as words and numbers. Which colour each MEANING wears is a ROW of
//! `ui-theme.toml`'s `side` map (`maps::side::BUY`, `maps::side::STOP_LOSS`...), one row per meaning even
//! where two meanings share a colour today, so a designer reads "stop-loss is the down red" off the table
//! rather than off a `match` in a panel.
//!
//! # What stays in the code, and what is here
//!
//! WHICH side applies is the site's own decision (`o.side > 0`, `pnl >= 0.0`, `delta < 0.0`): it stays where
//! the data is. What the site did next, `if buy { t.market.up } else { t.market.down }`, was repeated by about
//! sixty call sites; it is [`Pair::row`] now, written once, and the site reads the row:
//!
//! ```text
//! let col = Pair::GainLoss.text(pnl >= 0.0, &tokens);   // the row GAIN's `text`, or LOSS's
//! let (up, down) = Pair::RiseFall.colours(&tokens);     // a site that picks later
//! let tint = maps::side::STOP_LOSS.colour.resolve(&tokens); // a site that names its meaning outright
//! ```
//!
//! A [`Pair`] is the two rows a decision chooses between, FIRST (the buy, the long, the bid, the gain, the
//! positive...) or SECOND. [`Pair::rows`] is an exhaustive `match`, so a pair without its rows does not
//! compile; the tests below pin that every row of the map belongs to exactly one pair and that today's
//! rows are today's colours on every theme and every market set.

use egui::Color32;

use crate::components::Tokens;
use crate::maps::{self, MapRow};

/// Two meanings the market's two colours are dealt over. The first of each is the "up" side today.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pair {
    /// An order's side, or the aggressor of a print: [`maps::side::BUY`] and [`maps::side::SELL`].
    BuySell,
    /// A position's side: [`maps::side::LONG`] and [`maps::side::SHORT`].
    LongShort,
    /// A book's side: [`maps::side::BID`] and [`maps::side::ASK`].
    BidAsk,
    /// Money made or lost: [`maps::side::GAIN`] and [`maps::side::LOSS`].
    GainLoss,
    /// The sign of a number that is not money (a Greek, a metric, a Sharpe, a delta, a surprise):
    /// [`maps::side::POSITIVE`] and [`maps::side::NEGATIVE`].
    PositiveNegative,
    /// A bracket's two exits: [`maps::side::TAKE_PROFIT`] and [`maps::side::STOP_LOSS`].
    TpSl,
    /// A candle's direction, where a screen draws the two as examples: [`maps::side::RISE`] and
    /// [`maps::side::FALL`].
    RiseFall,
    /// A released figure against its forecast: [`maps::side::ABOVE_FORECAST`] and
    /// [`maps::side::BELOW_FORECAST`].
    ForecastAboveBelow,
    /// The two outcomes of an Up/Down market: [`maps::side::OUTCOME_UP`] and [`maps::side::OUTCOME_DOWN`].
    UpDownOutcome,
}

impl Pair {
    /// Every pair, in the order the TOML lists their rows.
    pub const ALL: [Pair; 9] = [
        Pair::BuySell,
        Pair::LongShort,
        Pair::BidAsk,
        Pair::GainLoss,
        Pair::PositiveNegative,
        Pair::TpSl,
        Pair::RiseFall,
        Pair::ForecastAboveBelow,
        Pair::UpDownOutcome,
    ];

    /// The pair's two rows, first and second.
    pub fn rows(self) -> (&'static MapRow, &'static MapRow) {
        match self {
            Pair::BuySell => (&maps::side::BUY, &maps::side::SELL),
            Pair::LongShort => (&maps::side::LONG, &maps::side::SHORT),
            Pair::BidAsk => (&maps::side::BID, &maps::side::ASK),
            Pair::GainLoss => (&maps::side::GAIN, &maps::side::LOSS),
            Pair::PositiveNegative => (&maps::side::POSITIVE, &maps::side::NEGATIVE),
            Pair::TpSl => (&maps::side::TAKE_PROFIT, &maps::side::STOP_LOSS),
            Pair::RiseFall => (&maps::side::RISE, &maps::side::FALL),
            Pair::ForecastAboveBelow => (&maps::side::ABOVE_FORECAST, &maps::side::BELOW_FORECAST),
            Pair::UpDownOutcome => (&maps::side::OUTCOME_UP, &maps::side::OUTCOME_DOWN),
        }
    }

    /// The row the site's decision picked: the first when `first` (the buy, the long, the bid, the gain...),
    /// else the second.
    pub fn row(self, first: bool) -> &'static MapRow {
        let (a, b) = self.rows();
        if first { a } else { b }
    }

    /// The picked row's graphic colour (a fill, a line, a marker, a bar) under the installed appearance.
    pub fn colour(self, first: bool, t: &Tokens) -> Color32 {
        self.row(first).colour.resolve(t)
    }

    /// The picked row's text colour (a word, a number) under the installed appearance.
    pub fn text(self, first: bool, t: &Tokens) -> Color32 {
        self.row(first).text.resolve(t)
    }

    /// Both graphic colours, first then second, for a site that decides later.
    pub fn colours(self, t: &Tokens) -> (Color32, Color32) {
        (self.colour(true, t), self.colour(false, t))
    }

    /// Both text colours, first then second, for a site that decides later.
    pub fn texts(self, t: &Tokens) -> (Color32, Color32) {
        (self.text(true, t), self.text(false, t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::market::MarketId;
    use crate::theme::ThemeId;

    fn tokens(theme: ThemeId, market: MarketId) -> Tokens {
        Tokens::from_appearance(&Appearance { theme, market, ..Appearance::default() })
    }

    /// The rows read through a pair are TODAY's colours on every theme and every market set: the first of a
    /// pair is the market's up, the second its down, as a graphic and as text. The expressions are the ones
    /// the `if buy { t.market.up } else { t.market.down }` ladders returned before the rows existed, so a
    /// change of the table changes this named test.
    #[test]
    fn a_pair_reads_the_markets_up_and_down_as_it_always_did() {
        for theme in ThemeId::ALL {
            for market in MarketId::ALL {
                let t = tokens(theme, market);
                for pair in Pair::ALL {
                    let at = format!("{theme:?} {market:?} {pair:?}");
                    assert_eq!(pair.colour(true, &t), t.market.up, "{at}: first, graphic");
                    assert_eq!(pair.colour(false, &t), t.market.down, "{at}: second, graphic");
                    assert_eq!(pair.text(true, &t), t.market.up_text, "{at}: first, text");
                    assert_eq!(pair.text(false, &t), t.market.down_text, "{at}: second, text");
                    assert_eq!(pair.colours(&t), (t.market.up, t.market.down), "{at}: colours");
                    assert_eq!(
                        pair.texts(&t),
                        (t.market.up_text, t.market.down_text),
                        "{at}: texts"
                    );
                }
            }
        }
    }

    /// Every row of the `side` map belongs to exactly one pair and every pair has two different rows of its
    /// own: a row nobody reads is a failure here, and a pair whose rows are not in the TOML does not compile.
    #[test]
    fn every_row_of_the_side_map_is_one_pairs_and_the_pairs_share_none() {
        let mut keys: Vec<&str> = Pair::ALL
            .iter()
            .flat_map(|p| {
                let (a, b) = p.rows();
                assert_ne!(a.key, b.key, "{p:?}: the same row twice");
                [a.key, b.key]
            })
            .collect();
        assert_eq!(keys.len(), 2 * Pair::ALL.len());
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), 2 * Pair::ALL.len(), "two pairs share a row");
        let mut listed: Vec<&str> = maps::side::ALL.iter().map(|r| r.key).collect();
        listed.sort_unstable();
        assert_eq!(keys, listed, "the pairs' rows are not the side map's rows");
    }

    /// A row drawn through a pair has both its colours (a side with no colour would paint nothing), and the
    /// pair's readers agree with the row they pick.
    #[test]
    fn a_pair_row_carries_both_of_its_colours_and_the_readers_pick_it() {
        let t = tokens(ThemeId::Graphite, MarketId::Classic);
        for pair in Pair::ALL {
            for first in [true, false] {
                let row = pair.row(first);
                let at = format!("{pair:?} {}", row.key);
                assert_ne!(row.colour, crate::roles::ColourRole::None, "{at}: colour");
                assert_ne!(row.text, crate::roles::ColourRole::None, "{at}: text");
                assert_eq!(pair.colour(first, &t), row.colour.resolve(&t), "{at}");
                assert_eq!(pair.text(first, &t), row.text.resolve(&t), "{at}");
            }
            assert_eq!(pair.row(true), pair.rows().0);
            assert_eq!(pair.row(false), pair.rows().1);
        }
    }
}
