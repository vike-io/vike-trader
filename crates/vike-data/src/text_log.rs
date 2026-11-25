//! Text history rows — the `kind=text` series: texts (news items, economic-calendar events, and the
//! later kinds: SEC filings, central-bank texts, a prediction market's rules) as THIS box received
//! them, kept so a study can ask what was known at a decision time.
//!
//! # Why it is recorded at all
//!
//! The datahub's tool feeds (`crates/vike-datahub/src/tool_feeds/mod.rs`) hold the last items of
//! sixteen RSS feeds and ForexFactory's current week in MEMORY. An RSS feed serves only its newest
//! items and the calendar only this week, so a day nobody recorded cannot be fetched later: the
//! history begins the day the recorder is switched on (`flags.record_texts`, decision 0143's
//! amendment of 2026-11-16). It is the user's own data on the user's own machine, never shipped.
//!
//! # Identity, and the two clocks
//!
//! A series is `kind=text/venue=<text kind>/symbol=<collector>`: the text kind ([`TEXT_KIND_NEWS`],
//! [`TEXT_KIND_ECONOMIC_CALENDAR`]; the vocabulary is OPEN, a later kind is a new word over the same
//! columns) and the collector that fetched it. The kind is ALSO the [`TextRow::kind`] column, so a row
//! read out of several series stays self-describing.
//!
//! The store's `ts` column is [`TextRow::received_ms`]: when this box first HELD the row, in UTC
//! milliseconds. [`TextRow::published_ms`] is what the source SAYS (an RSS `pubDate`, a calendar
//! event's scheduled time, a filing's acceptance time). A point-in-time read is therefore a plain `ts`
//! range that ends at the decision time ([`crate::HistoryStore::texts_as_of`]); a study must never
//! select on `published_ms`, which a source can back-date and which says nothing about when the text
//! could have been read. A text fetched AFTER the fact (an archive) is received when it was fetched,
//! never when it was published: the history then simply begins later. The book plan's rule for a text
//! downloaded after the fact allows two exceptions. A source whose publication time is its own
//! authenticated stamp that nothing precedes: an SEC filing is public at its EDGAR ACCEPTANCE time and
//! never before, so an archived filing is received at that time
//! (`crates/vike-backfill/src/sec_edgar/texts.rs`). And a closed prediction market's archive: its
//! rules are received at the market's CLOSE, never at its start, because the archive serves today's
//! wording, and its final outcome is a later row of the same text received at its RESOLUTION, never
//! at the close (`TEXT_KIND_POLYMARKET_RULES`; `crates/bridges/polymarket/src/catalog/rules.rs`'s
//! module doc says why).
//!
//! # One row per thing seen, a revision as a new row, one id per text
//!
//! [`TextRow::text_id`] is what makes two rows the SAME text: the key a recorder stores a text once
//! under, the key a study folds revisions by, and the key a derived series (an NLP score of a text)
//! joins on. [`TextRow::revision`] is what makes a later row of it worth storing: a calendar event's
//! numbers (the `actual` that lands at release is a row of its own, and the forecast before it stays
//! what it was), the wording of any later kind, nothing for a news item. The last row of an id received
//! at or before a time is the version known then.
//!
//! # Columns a later kind fills
//!
//! [`TextRow::source_id`] (the source's own id: an SEC accession number, a market id),
//! [`TextRow::subject`] (what the text is about: a ticker, a CIK, a market), [`TextRow::period`] (the
//! period it reports on), [`TextRow::body`] (the full text, beyond `summary`) and [`TextRow::amends`]
//! (the `source_id` of the text it corrects: a 10-K/A's 10-K). Every column is a string that is EMPTY
//! where a kind has nothing for it, so a new kind is a new word, never a schema change.
//!
//! Pure data, no I/O. The Parquet codec is `vike-store`'s, beside the other kinds'.

use serde::{Deserialize, Serialize};

/// The text kind of a news item (an RSS headline and its description).
pub const TEXT_KIND_NEWS: &str = "news";

/// The text kind of an economic-calendar event (a scheduled release and its numbers).
pub const TEXT_KIND_ECONOMIC_CALENDAR: &str = "economic_calendar";

/// The text kind of a prediction market's RULES: one Polymarket market as Gamma states it. Its columns:
/// `source_id` the market's condition id (so every row of one market is one text), `subject` its outcome
/// token ids in outcome order, space-separated (the symbols vike records and trades it under), `title`
/// the question, `body` the rules (Gamma's `description`), `summary` the resolution source, `url` the
/// market's page, `period` its scheduled end as Gamma spells it, `tags` its outcomes in order,
/// `published_ms` its start, `actual` the final outcome once it has resolved (the winning outcome, or
/// every outcome's payout when none pays whole), and `amends` the condition id again on a LATER row of a
/// market already stored: its CHANGED rules (the recorder), or its outcome (the archive). Rows are
/// written by the datahub alone, into two series that a study reads side by side:
/// [`POLYMARKET_RULES_RECORDER_COLLECTOR`]'s and
/// [`POLYMARKET_RULES_ARCHIVE_COLLECTOR`]'s (both read through `crate::source::MarketRulesSource`).
pub const TEXT_KIND_POLYMARKET_RULES: &str = "polymarket_rules";

/// The series of [`TEXT_KIND_POLYMARKET_RULES`] the datahub's RECORDER writes live: the market of every token it
/// records, received when it first read it, and again (with `amends`) each time the wording it reads changes.
pub const POLYMARKET_RULES_RECORDER_COLLECTOR: &str = "recorder";

/// The series of [`TEXT_KIND_POLYMARKET_RULES`] the datahub's BACKFILL writes from the venue's archive of closed
/// markets: per market, its rules received at its close and, once it has resolved, its final outcome as a later
/// row (`amends`) received at its resolution.
pub const POLYMARKET_RULES_ARCHIVE_COLLECTOR: &str = "archive";

/// What separates the tags inside [`TextRow::tags`]: one stored string per row, so a tag never holds
/// it ([`TextRow::joined_tags`] turns one inside a tag into a space).
pub const TEXT_TAG_SEPARATOR: char = '\n';

/// What separates the parts of a [`TextRow::text_id`] and a [`TextRow::revision`]: the unit separator,
/// which no feed text carries, so two different part lists never spell one id.
const PART_SEPARATOR: char = '\u{1f}';

/// One text as this box received it, persisted as the `kind=text` HistoryStore series. Every string
/// field is EMPTY when the source gave nothing for it, and a field that belongs to another text kind
/// (a news item's `currency`, a calendar event's `url`) is empty too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRow {
    /// When this box first held this row (this revision of it), epoch milliseconds UTC. The store's
    /// `ts`: what a point-in-time read selects on.
    pub received_ms: i64,
    /// When the source says it was published, epoch milliseconds UTC: an RSS item's `pubDate`, a
    /// calendar event's scheduled time, a filing's acceptance time. `0` when the source gave none.
    pub published_ms: i64,
    /// The text kind ([`TEXT_KIND_NEWS`], [`TEXT_KIND_ECONOMIC_CALENDAR`], a later word), the series'
    /// `venue` too.
    pub kind: String,
    /// The provider's name as its collector spells it (`CoinDesk`, `ForexFactory`).
    pub source: String,
    /// The source's OWN id of the text (an SEC accession number, a market id); when present it is the
    /// text's id ([`TextRow::text_id`]).
    pub source_id: String,
    /// What the text is about: a ticker, a CIK, a market id.
    pub subject: String,
    /// The period the text reports on, as the source spells it (`2026Q3`, `FY2025`).
    pub period: String,
    /// The headline, or the calendar event's name, or the document's title.
    pub title: String,
    /// The plain-text description (HTML stripped, clipped by the collector).
    pub summary: String,
    /// The full text, beyond `summary`.
    pub body: String,
    /// The link to the original.
    pub url: String,
    /// The feed's market bucket (`crypto`, `forex`, `stocks`).
    pub market: String,
    /// The text's tags, joined by [`TEXT_TAG_SEPARATOR`]; [`TextRow::tag_list`] splits them.
    pub tags: String,
    /// A calendar event's currency code (`USD`).
    pub currency: String,
    /// A calendar event's impact word (`High`, `Medium`, `Low`, `Holiday`).
    pub impact: String,
    /// A calendar event's consensus forecast, as the feed spells it.
    pub forecast: String,
    /// A calendar event's previous release, as the feed spells it.
    pub previous: String,
    /// A calendar event's actual release, as the feed spells it; empty until it lands.
    pub actual: String,
    /// The `source_id` of the text this one corrects (a 10-K/A's 10-K).
    pub amends: String,
}

impl TextRow {
    /// An empty row of `kind` from `source`, received at `received_ms`: the start of a builder's
    /// struct update, so a collector names only the fields its kind has.
    pub fn new(kind: &str, source: &str, received_ms: i64) -> Self {
        Self {
            received_ms,
            published_ms: 0,
            kind: kind.to_string(),
            source: source.to_string(),
            source_id: String::new(),
            subject: String::new(),
            period: String::new(),
            title: String::new(),
            summary: String::new(),
            body: String::new(),
            url: String::new(),
            market: String::new(),
            tags: String::new(),
            currency: String::new(),
            impact: String::new(),
            forecast: String::new(),
            previous: String::new(),
            actual: String::new(),
            amends: String::new(),
        }
    }

    /// `tags` as one stored string: each tag's own separators turned into spaces, so the split gives
    /// back exactly the tags that went in.
    pub fn joined_tags<S: AsRef<str>>(tags: &[S]) -> String {
        let cleaned: Vec<String> = tags
            .iter()
            .map(|t| t.as_ref().replace(TEXT_TAG_SEPARATOR, " "))
            .filter(|t| !t.is_empty())
            .collect();
        cleaned.join(&TEXT_TAG_SEPARATOR.to_string())
    }

    /// The tags [`TextRow::tags`] holds, in their order; none for an empty string.
    pub fn tag_list(&self) -> impl Iterator<Item = &str> {
        self.tags.split(TEXT_TAG_SEPARATOR).filter(|t| !t.is_empty())
    }

    /// **The text's id**: what makes two rows the SAME text, the same for every revision of it and
    /// stable across processes (it is built from the row, never from a counter or a clock). Its parts:
    /// the kind and the source, then the source's own id when it gave one; else, for a calendar event,
    /// its currency, title and scheduled time (a rescheduled event is a new one); else the url, or the
    /// title when there is no url.
    pub fn text_id(&self) -> String {
        let published = self.published_ms.to_string();
        let mut parts = vec![self.kind.as_str(), self.source.as_str()];
        if !self.source_id.is_empty() {
            parts.push(self.source_id.as_str());
        } else if self.kind == TEXT_KIND_ECONOMIC_CALENDAR {
            parts.extend([self.currency.as_str(), self.title.as_str(), published.as_str()]);
        } else if self.url.is_empty() {
            parts.push(self.title.as_str());
        } else {
            parts.push(self.url.as_str());
        }
        joined(&parts)
    }

    /// **What makes a later row of the same text a REVISION worth storing**: nothing for a news item
    /// (it is stored once, whatever its feed does to it later); a calendar event's impact, forecast,
    /// previous and actual; the wording (title, summary, body) of any other kind.
    pub fn revision(&self) -> String {
        if self.kind == TEXT_KIND_NEWS {
            return String::new();
        }
        if self.kind == TEXT_KIND_ECONOMIC_CALENDAR {
            return joined(&[
                self.impact.as_str(),
                self.forecast.as_str(),
                self.previous.as_str(),
                self.actual.as_str(),
            ]);
        }
        joined(&[self.title.as_str(), self.summary.as_str(), self.body.as_str()])
    }
}

/// `parts` joined by [`PART_SEPARATOR`].
fn joined(parts: &[&str]) -> String {
    parts.join(&PART_SEPARATOR.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str) -> TextRow {
        TextRow {
            published_ms: 1,
            title: "T".to_string(),
            url: "https://example.test/a".to_string(),
            currency: "USD".to_string(),
            impact: "High".to_string(),
            forecast: "1".to_string(),
            previous: "0".to_string(),
            ..TextRow::new(kind, "S", 2)
        }
    }

    #[test]
    fn tags_round_trip_and_a_separator_inside_a_tag_becomes_a_space() {
        let mut r = row(TEXT_KIND_NEWS);
        r.tags = TextRow::joined_tags(&["Markets", "", "two\nlines"]);
        assert_eq!(r.tag_list().collect::<Vec<_>>(), ["Markets", "two lines"]);
        r.tags = TextRow::joined_tags::<&str>(&[]);
        assert_eq!(r.tag_list().count(), 0, "no tags is an empty string");
    }

    #[test]
    fn a_news_item_is_its_source_and_url_and_never_revises() {
        let a = row(TEXT_KIND_NEWS);
        let mut b =
            TextRow { received_ms: 99, title: "edited".into(), actual: "5".into(), ..a.clone() };
        assert_eq!(a.text_id(), b.text_id(), "same source and url: the same item");
        assert_eq!(a.revision(), b.revision(), "a news item has no revision");
        b.url.clear();
        assert_ne!(a.text_id(), b.text_id(), "no url: the title is the key");
        let c = TextRow { source: "Other".into(), ..a.clone() };
        assert_ne!(a.text_id(), c.text_id(), "another source's same url is another item");
        assert_eq!(
            a.text_id(),
            "news\u{1f}S\u{1f}https://example.test/a",
            "the id's spelling is frozen: derived series join on it"
        );
    }

    #[test]
    fn a_calendar_event_revises_on_its_numbers_and_is_new_when_rescheduled() {
        let a = row(TEXT_KIND_ECONOMIC_CALENDAR);
        let released = TextRow { actual: "1.2".into(), ..a.clone() };
        assert_eq!(a.text_id(), released.text_id(), "the same event");
        assert_ne!(a.revision(), released.revision(), "the actual landing is a revision");
        let moved = TextRow { published_ms: 7, ..a.clone() };
        assert_ne!(a.text_id(), moved.text_id(), "a rescheduled event is a new one");
        assert_eq!(a.text_id(), "economic_calendar\u{1f}S\u{1f}USD\u{1f}T\u{1f}1");
    }

    /// A later kind with the source's own id: that id is the text's, its wording is its revision.
    #[test]
    fn a_source_id_is_the_id_and_a_later_kinds_wording_is_its_revision() {
        let filing = TextRow {
            source_id: "0000320193-26-000077".into(),
            subject: "AAPL".into(),
            period: "FY2026".into(),
            body: "Item 1. Business".into(),
            ..TextRow::new("sec_10k", "EDGAR", 5)
        };
        assert_eq!(filing.text_id(), "sec_10k\u{1f}EDGAR\u{1f}0000320193-26-000077");
        let moved = TextRow { url: "https://elsewhere.test".into(), ..filing.clone() };
        assert_eq!(filing.text_id(), moved.text_id(), "the url no longer decides");
        let reworded = TextRow { body: "Item 1. Business (restated)".into(), ..filing.clone() };
        assert_ne!(filing.revision(), reworded.revision(), "a changed text is a revision");
    }
}
