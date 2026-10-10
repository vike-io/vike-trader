//! `VenueLink`, the one spelling of reading and writing a row's link to its venue, and `venue_is`,
//! the filter on the number.

/// **How one table's link to a venue is READ and WRITTEN on this store** — the one spelling every
/// statement in this crate uses, so the shape of the link is decided in one place.
///
/// Three shapes are spelled:
///
/// | the table has | reads | writes |
/// |---|---|---|
/// | `venue_id` and the text `venue` | the number, through `venue`; the text only for a row whose number names no `venue` row | both |
/// | `venue_id` only | the number | the number |
/// | the text `venue` only | the text: there is no number to read | the text |
///
/// The shipped shape (`crate::schema::DDL`) is the second row for `account`, `credential` and
/// `venue_setting`, and the first for `venue_arming`, which keeps its text column until that table
/// is deleted. No statement here assumes a shape: each asks [`VenueLink::of`] for the one the store
/// has. ⚠ The third row describes a store older than the `venue_id` columns; no store in the field
/// holds it, and nothing upgrades one any more.
///
/// ⚠ **The text fallback is not a second source.** A row reaches it only when its `venue_id` names
/// no `venue` row: on the shipped shape that is a number left dangling by a hand edit with foreign
/// keys off, which the LEFT join below keeps. Where a statement FILTERS by venue through the link it
/// uses [`VenueLink::named`] (`crate::db::set_venue_account_id`'s book-holder check); every other
/// statement filters with [`venue_is`], on the number alone.
///
/// ⚠ **The join is a LEFT join in both shapes that carry a number.** An inner join DROPS a row
/// whose number is NULL or names no `venue` row, and `credential.venue_id` is NULL by design on
/// every account-scoped row: a reader that ever read `credential` through this link would lose those
/// rows without a word, and an arming row lost that way is a `max_exposure` figure lost, which means
/// UNBOUNDED. A LEFT join keeps the row and hands its reader a NULL name, which a `String` column
/// refuses loudly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VenueLink {
    /// A SELECT expression naming the venue of the aliased row, e.g. `COALESCE(v.name, a.venue)`.
    pub name: String,
    /// The join clause that expression needs; empty when it needs none.
    pub join: String,
    /// The INSERT column list for the link: `venue, venue_id` or `venue_id`.
    pub columns: &'static str,
    has_text: bool,
}

impl VenueLink {
    /// Ask the store which shape `table` has. `alias` is the alias the caller's statement gives
    /// the table (`a`, `t`, …), and the venue join uses `v`.
    pub(crate) fn of(
        conn: &rusqlite::Connection,
        table: &str,
        alias: &str,
    ) -> rusqlite::Result<VenueLink> {
        let has_id = crate::settings::has_column(conn, table, "venue_id")?;
        let has_text = crate::settings::has_column(conn, table, "venue")?;
        let by_number = format!("LEFT JOIN venue v ON v.id = {alias}.venue_id");
        let (name, join) = match (has_id, has_text) {
            (true, true) => (format!("COALESCE(v.name, {alias}.venue)"), by_number),
            (true, false) => ("v.name".to_string(), by_number),
            (false, _) => (format!("{alias}.venue"), String::new()),
        };
        let columns = if has_text { "venue, venue_id" } else { "venue_id" };
        Ok(VenueLink { name, join, columns, has_text })
    }

    /// The VALUES fragment matching [`VenueLink::columns`] for a venue NAME bound at `param`
    /// (`?1`, …). The number is looked up in the same statement, so a writer never holds a
    /// `venue_id` it could get wrong.
    pub(crate) fn values(&self, param: &str) -> String {
        let id = format!("(SELECT id FROM venue WHERE name = {param})");
        if self.has_text { format!("{param}, {id}") } else { id }
    }

    /// The filter for rows of the venue NAMED at `param`, comparing the very expression
    /// [`VenueLink::name`] selects, so it finds exactly the rows a reader names that venue: by the
    /// number where a row's number names a `venue` row, by the text only where it does not. The
    /// statement must carry [`VenueLink::join`].
    pub(crate) fn named(&self, param: &str) -> String {
        format!("{} = {param}", self.name)
    }
}

/// The filter a statement uses to find rows of ONE venue by its name bound at `param`, keyed on the
/// number. `column` is the table's `venue_id` column as the statement spells it.
pub(crate) fn venue_is(column: &str, param: &str) -> String {
    format!("{column} = (SELECT id FROM venue WHERE name = {param})")
}
