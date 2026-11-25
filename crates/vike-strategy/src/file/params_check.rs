//! The load-time checks of a `[strategy.params]` table against a header. Each finding renders in
//! the words of the registry check it replaced: the words MOVED here from the deleted registry's
//! keys, routes and gates tables (`unknown_params`, `mistyped_params`, `misrouted_params`,
//! `Gate::unmet`); the sentence around it is the consumer's.

use vike_model::params::Params;

use super::{
    ParamTypeError, RouteKind, RouteMismatch, StrategyFileHeader, StrategyFileNeed,
    StrategyFileRoutes,
};

/// The "it reads" tail of a file whose header declares no `[params]`: it accepts no key.
pub(super) const NO_PARAMS: &str = "nothing: declare it in the file's header";

impl StrategyFileHeader {
    /// The keys in `params` the header does not declare: the ones that would configure nothing.
    /// Sorted. A header with no `[params]` reports EVERY key (the file accepts none).
    pub fn unknown_params(&self, params: &Params) -> Vec<String> {
        let mut unknown: Vec<String> =
            params.keys().filter(|k| self.param(k.as_str()).is_none()).cloned().collect();
        unknown.sort();
        unknown
    }

    /// The declared keys in `params` whose VALUE is of a type the reader cannot take, sorted by
    /// key. An absent key is not judged (absent is the default, which is legal).
    pub fn mistyped_params(&self, params: &Params) -> Vec<ParamTypeError> {
        let mut bad: Vec<ParamTypeError> = self
            .params
            .iter()
            .filter_map(|(key, p)| {
                let v = params.get(key)?;
                (!p.r#type.accepts(v)).then(|| ParamTypeError {
                    key: key.clone(),
                    expected: p.r#type.expected().to_string(),
                    got: v.type_str().to_string(),
                })
            })
            .collect();
        bad.sort_by(|a, b| a.key.cmp(&b.key));
        bad
    }

    /// The declared keys in `params` whose value is of the right type and outside the declared
    /// `range`, sorted by key: `` `rungs` wants a value in [1, 500], got 900 ``. A value of the
    /// wrong type is [`Self::mistyped_params`]' finding.
    pub fn out_of_range_params(&self, params: &Params) -> Vec<ParamTypeError> {
        let mut bad: Vec<ParamTypeError> = self
            .params
            .iter()
            .filter_map(|(key, p)| {
                let [low, high] = p.range?;
                let n = params.get(key).filter(|v| p.r#type.accepts(v))?.as_f64()?;
                (!(low..=high).contains(&n)).then(|| ParamTypeError {
                    key: key.clone(),
                    expected: format!("a value in [{low}, {high}]"),
                    got: n.to_string(),
                })
            })
            .collect();
        bad.sort_by(|a, b| a.key.cmp(&b.key));
        bad
    }

    /// The route keys in `params` that name a market OTHER than the single-leg mount's own
    /// `(venue, symbol)`: the ones the mount would silently override. Sorted by key.
    ///
    /// Empty for a `multi_leg` header. A `symbol` route that is absent or empty is not reported, a
    /// value of the wrong type is [`Self::mistyped_params`]' finding, and a `venue_map` row is
    /// legal only when it names this mount's own route outright (`<mount symbol> = "<mount
    /// venue>"`); a row whose value is not a string is reported too.
    pub fn misrouted_params(
        &self,
        params: &Params,
        venue: &str,
        symbol: &str,
    ) -> Vec<RouteMismatch> {
        if self.strategy.routes == StrategyFileRoutes::MultiLeg {
            return Vec::new();
        }
        let mut out: Vec<RouteMismatch> = Vec::new();
        for (key, p) in &self.params {
            let (Some(kind), Some(v)) = (p.route, params.get(key)) else {
                continue;
            };
            match kind {
                RouteKind::Symbol => {
                    if let Some(s) = v.as_str()
                        && !s.is_empty()
                        && s != symbol
                    {
                        out.push(RouteMismatch {
                            key: key.clone(),
                            named: format!("instrument {s:?}"),
                            mounted: format!("{symbol:?}"),
                        });
                    }
                }
                RouteKind::Venue => {
                    if let Some(s) = v.as_str()
                        && s != venue
                    {
                        out.push(RouteMismatch {
                            key: key.clone(),
                            named: format!("venue {s:?}"),
                            mounted: format!("{venue:?}"),
                        });
                    }
                }
                RouteKind::VenueMap => {
                    if let Some(rows) = v.as_table() {
                        for (row_symbol, row_value) in rows.iter() {
                            if row_symbol == symbol && row_value.as_str() == Some(venue) {
                                continue;
                            }
                            let named = match row_value.as_str() {
                                Some(row_venue) => format!("{row_symbol:?} on venue {row_venue:?}"),
                                None => format!(
                                    "{row_symbol:?} on a {} (which this reader DROPS)",
                                    row_value.type_str()
                                ),
                            };
                            out.push(RouteMismatch {
                                key: format!("{key}.{row_symbol}"),
                                named,
                                mounted: format!("{symbol:?} on venue {venue:?}"),
                            });
                        }
                    }
                }
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    /// The `needs` of `key` that FAIL against `rows` (the file's `echo`: what each knob RESOLVED
    /// to), each rendered `other=value`, or `other=(absent)` when the rows do not carry it. Empty:
    /// the key really is consumed, or it declares no `needs` (or is not declared).
    pub fn unmet_needs(&self, key: &str, rows: &[(&str, String)]) -> Vec<String> {
        let Some(p) = self.param(key) else {
            return Vec::new();
        };
        p.needs.iter().flat_map(|(other, need)| need.unmet(other, rows)).collect()
    }

    /// What the file reads, as `key (type)` pairs in the header's order: the "it reads" tail of a
    /// params refusal. A header with no `[params]` reads "nothing: declare it in the file's
    /// header".
    pub fn readable_keys(&self) -> String {
        if self.params.is_empty() {
            return NO_PARAMS.to_string();
        }
        self.params
            .iter()
            .map(|(key, p)| format!("{key} ({})", p.r#type.expected()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl StrategyFileNeed {
    /// This need of the OTHER key `key`, judged against `rows`: empty when met, else the one
    /// `key=value` (or `key=(absent)`) that fails it.
    pub fn unmet(&self, key: &str, rows: &[(&str, String)]) -> Vec<String> {
        let Some(v) = rows.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str()) else {
            return vec![format!("{key}=(absent)")];
        };
        let met = match self {
            StrategyFileNeed::OneOf(wanted) => wanted.iter().any(|w| w == v),
            StrategyFileNeed::Positive => v.parse::<f64>().is_ok_and(|n| n > 0.0),
        };
        if met { Vec::new() } else { vec![format!("{key}={v}")] }
    }
}
