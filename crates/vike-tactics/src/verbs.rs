//! The builders a verb of [`crate::Tactics`] returns: each states one order, and a call chains only
//! what differs from the rule's header (decision 0269's agreed shape). A builder records; the order
//! is sent when the rule's next verb starts or its hook returns, in call order.

use vike_model::OrderSide;

/// A trailing exit: nothing can fire until the price is `after_ticks` past the lots' entry, and from
/// there it follows the best price by `by_ticks`. With `by_ticks` 0 the exit IS the first touch of
/// that level, so it rests there as an if-touched order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TacticsTrail {
    pub after_ticks: f64,
    pub by_ticks: f64,
}

/// A level an exit states: a price, or a percent of the lots' entry price.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Level {
    At(f64),
    /// In percent as a rule states it: `1.0` is 1%.
    Percent(f64),
}

/// An entry under a name ([`crate::Tactics::entry`]): a market order unless it states a level.
#[derive(Debug, Clone, PartialEq)]
pub struct TacticsEntry {
    pub(crate) name: String,
    pub(crate) side: OrderSide,
    pub(crate) size: f64,
    pub(crate) stop: Option<f64>,
    pub(crate) limit: Option<f64>,
    /// [`TacticsEntry::chase`]: pegged to its own side of the touch.
    pub(crate) chase: bool,
    /// [`TacticsEntry::iceberg`]'s visible size.
    pub(crate) display: Option<f64>,
    pub(crate) group: Option<String>,
    /// [`TacticsEntry::here`]: fills at this bar's close.
    pub(crate) here: bool,
}

/// A free order ([`crate::Tactics::order`]): neither an entry nor a close. It fills its stated size
/// whatever is held then, so it can close, add to or open a position; the pyramiding limit and an
/// opposite position do not touch it.
#[derive(Debug, Clone, PartialEq)]
pub struct TacticsOrder {
    pub(crate) entry: TacticsEntry,
}

/// A close of the lots of a name, or of the whole position ([`crate::Tactics::close`],
/// [`crate::Tactics::close_all`]), at the next fill a market order gets.
#[derive(Debug, Clone, PartialEq)]
pub struct TacticsClose {
    /// `None`: the whole position.
    pub(crate) lots: Option<String>,
    pub(crate) size: Option<f64>,
    pub(crate) here: bool,
}

/// An exit protecting the lots of a name ([`crate::Tactics::exit`]): a stop, a target and a
/// trailing leg, each optional, each cancelling the others when it fills. Every method takes a
/// plain value and a leg no method states is not sent, so a level that is sometimes missing is a
/// condition where the rule states it: `if let Some(s) = stop { exit.stop(s); }`.
#[derive(Debug, Clone, PartialEq)]
pub struct TacticsExit {
    pub(crate) name: String,
    pub(crate) lots: String,
    pub(crate) size: Option<f64>,
    pub(crate) take: Option<Level>,
    pub(crate) stop: Option<Level>,
    pub(crate) trail: Option<TacticsTrail>,
    pub(crate) group: Option<String>,
}

impl TacticsEntry {
    pub(crate) fn new(name: &str, side: OrderSide, size: f64) -> TacticsEntry {
        let (stop, limit, chase, display, group) = (None, None, false, None, None);
        let here = false;
        TacticsEntry {
            name: name.to_string(),
            side,
            size,
            stop,
            limit,
            chase,
            display,
            group,
            here,
        }
    }

    /// The size, instead of the header's. An entry of size 0 only closes the opposite position, and
    /// with nothing to close takes the entry resting under its name off the book.
    pub fn size(&mut self, size: f64) -> &mut Self {
        self.size = size;
        self
    }

    /// Rests as a stop at `level` (snapped as a stop is); with [`TacticsEntry::limit`] too, a stop-limit.
    pub fn stop(&mut self, level: f64) -> &mut Self {
        self.stop = Some(level);
        self
    }

    /// Rests as a limit at `level` (snapped as a limit is); with [`TacticsEntry::stop`] too, a stop-limit
    /// filling no worse than `level`.
    pub fn limit(&mut self, level: f64) -> &mut Self {
        self.limit = Some(level);
        self
    }

    /// Rests PEGGED to its own side of the touch, the best bid for a buy and the best ask for a sell
    /// (`vike_model::OrderPlacement::Pegged`, `vike_model::OrderPegAnchor::Best` with no offset), its price
    /// following the touch as it moves. It meets the venue's capability row as any pegged order
    /// does. It states no level: with [`TacticsEntry::stop`], [`TacticsEntry::limit`] or [`TacticsEntry::iceberg`] too,
    /// the order is refused by name (a `Denied` order event, `vike_model::OrderBroker::deny`).
    pub fn chase(&mut self) -> &mut Self {
        self.chase = true;
        self
    }

    /// An ICEBERG: of the limit's size only `visible` shows (`vike_model::OrderPlacement::Iceberg`, the
    /// order's `display_quantity`). It needs [`TacticsEntry::limit`] and no stop. A `visible` not above zero (a
    /// hidden order) is refused by name, as an iceberg without a limit is (a `Denied` order event).
    pub fn iceberg(&mut self, visible: f64) -> &mut Self {
        self.display = Some(visible);
        self
    }

    /// Puts the order in `group`: when any member fills, every other member is cancelled.
    pub fn group(&mut self, group: &str) -> &mut Self {
        self.group = Some(group.to_string());
        self
    }

    /// Fills at THIS bar's close, not with the next market fill (`vike_model::OrderBroker::place_at_close`),
    /// as [`TacticsClose::here`] does. A market entry only: with a level, a chase or an iceberg it is refused by
    /// name (a `Denied` order event, `vike_model::OrderBroker::deny`). It goes alone, so an exit issued with it
    /// waits for its fill.
    pub fn here(&mut self) -> &mut Self {
        self.here = true;
        self
    }
}

impl TacticsOrder {
    /// The size, instead of the header's.
    pub fn size(&mut self, size: f64) -> &mut Self {
        self.entry.size(size);
        self
    }

    /// Rests as a stop at `level`.
    pub fn stop(&mut self, level: f64) -> &mut Self {
        self.entry.stop(level);
        self
    }

    /// Rests as a limit at `level`.
    pub fn limit(&mut self, level: f64) -> &mut Self {
        self.entry.limit(level);
        self
    }

    /// Rests pegged to its own side of the touch ([`TacticsEntry::chase`]).
    pub fn chase(&mut self) -> &mut Self {
        self.entry.chase();
        self
    }

    /// An iceberg showing `visible` of the limit's size ([`TacticsEntry::iceberg`]).
    pub fn iceberg(&mut self, visible: f64) -> &mut Self {
        self.entry.iceberg(visible);
        self
    }

    /// Puts the order in `group`.
    pub fn group(&mut self, group: &str) -> &mut Self {
        self.entry.group(group);
        self
    }
}

impl TacticsClose {
    /// Closes at most `size` of the lots.
    pub fn size(&mut self, size: f64) -> &mut Self {
        self.size = Some(size);
        self
    }

    /// Fills at THIS bar's close, not with the next market fill
    /// (`vike_model::OrderBroker::place_at_close`).
    pub fn here(&mut self) -> &mut Self {
        self.here = true;
        self
    }
}

impl TacticsExit {
    /// The lots it protects, instead of those of its own name.
    pub fn from(&mut self, lots: &str) -> &mut Self {
        self.lots = lots.to_string();
        self
    }

    /// How much of the lots it closes, instead of all of them.
    pub fn size(&mut self, size: f64) -> &mut Self {
        self.size = Some(size);
        self
    }

    /// A stop at `level`.
    pub fn stop(&mut self, level: f64) -> &mut Self {
        self.stop = Some(Level::At(level));
        self
    }

    /// A target (a limit) at `level`.
    pub fn take(&mut self, level: f64) -> &mut Self {
        self.take = Some(Level::At(level));
        self
    }

    /// A stop `pct` percent from the lots' entry price, against the position.
    pub fn stop_pct(&mut self, pct: f64) -> &mut Self {
        self.stop = Some(Level::Percent(pct));
        self
    }

    /// A target `pct` percent from the lots' entry price, with the position.
    pub fn take_pct(&mut self, pct: f64) -> &mut Self {
        self.take = Some(Level::Percent(pct));
        self
    }

    /// A trailing leg.
    pub fn trail(&mut self, trail: TacticsTrail) -> &mut Self {
        self.trail = Some(trail);
        self
    }

    /// Puts the exit in `group`: a fill of any member cancels every other, and exits of one group
    /// hold no share of an entry against one another.
    pub fn group(&mut self, group: &str) -> &mut Self {
        self.group = Some(group.to_string());
        self
    }

    /// Whether a level is stated as a percent of an entry price still to be known.
    pub(crate) fn relative(&self) -> bool {
        matches!(self.take, Some(Level::Percent(_))) || matches!(self.stop, Some(Level::Percent(_)))
    }
}
