//! The trait a strategy rule implements: its header, and what it does on a bar and after its own
//! fill. The tactics object it receives is named `strategy` by convention (decision 0269's agreed
//! shape); it is not the `vike_model::Strategy` trait, which [`crate::RuleRun`] implements for it.

use vike_marketdata::Bar;

use crate::header::RuleDefaults;
use crate::tactics::Tactics;

/// One strategy rule, written in the order language. Object-safe, so a table of rules can hold
/// `Box<dyn Rule>`.
pub trait Rule {
    /// The defaults the rule's verbs take where a call states none.
    fn defaults(&self) -> RuleDefaults;

    /// Runs on EVERY bar the run is fed, so indicators warm up; the order verbs do nothing outside
    /// the run's trading window ([`crate::RuleRun::trading`]), so a rule never has to ask.
    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar);

    /// Runs again right after one of the rule's own fills INSIDE `bar` (a fill stamped with the bar
    /// being walked), reading `bar` whole. What it issues works from the next point of that walk.
    /// Nothing runs after a fill at a close. Off unless a rule implements it.
    fn after_fill(&mut self, _strategy: &mut Tactics<'_>, _bar: &Bar) {}

    /// The stored series the rule reads beside its own bars ([`crate::Tactics::input`]), each by its
    /// name (decision 0271): `INTERVAL` for the run's own symbol at another bar length (`"1d"`),
    /// `VENUE:SYMBOL:INTERVAL` for another series, `LANE:VENUE:SYMBOL:INTERVAL` for a `source=` lane
    /// such as a user's CSV import. [`crate::RuleRun`] hands them to the run
    /// (`vike_model::Strategy::inputs`), which loads only these: a name the rule reads but does not
    /// declare reads nothing. Read once, before the run. Default: none.
    fn inputs(&self) -> Vec<String> {
        Vec::new()
    }
}

/// A boxed rule is a rule: what a table of rules hands out (`Box<dyn Rule + Send>`) goes through
/// [`crate::RuleRun`] as it is.
impl<R: Rule + ?Sized> Rule for Box<R> {
    fn defaults(&self) -> RuleDefaults {
        (**self).defaults()
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        (**self).on_bar(strategy, bar);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        (**self).after_fill(strategy, bar);
    }

    fn inputs(&self) -> Vec<String> {
        (**self).inputs()
    }
}
