//! The live smokes' shared guards, run offline over doubles: the leverage round trip the binance,
//! bybit and okx leverage smokes each carried a copy of, and the cancel guard of the fine-tick and
//! restore smokes.

use std::cell::{Cell, RefCell};
use std::sync::Mutex;

use super::*;
use crate::leverage_read::HeldLeverage;
use crate::transport::VenueApiError;

/// A venue that holds `held` and reports `position_open`; each write moves the figure it holds.
struct Venue {
    held: Cell<f64>,
    position_open: bool,
    writes: RefCell<Vec<f64>>,
}

impl Venue {
    fn holding(held: f64, position_open: bool) -> Self {
        Self { held: Cell::new(held), position_open, writes: RefCell::default() }
    }

    fn write(&self, figure: f64) -> Result<(), String> {
        self.writes.borrow_mut().push(figure);
        self.held.set(figure);
        Ok(())
    }
}

impl LeverageReader for Venue {
    fn check_account(&self) -> Result<(), String> {
        Ok(())
    }
    fn venue_max(&self) -> Result<Option<f64>, String> {
        Ok(Some(50.0))
    }
    fn held(&self) -> Result<HeldLeverage, String> {
        Ok(HeldLeverage { held: self.held.get(), position_open: self.position_open })
    }
}

#[test]
fn the_round_trip_writes_a_different_figure_reads_it_back_and_restores_the_prior() {
    let venue = Venue::holding(2.0, false);
    let answer =
        leverage_write_read_back(&venue, "BTCUSDT", "DEMO", "leverage", |x| venue.write(x));
    assert_eq!(answer, Ok((2.0, 3.0)));
    assert_eq!(*venue.writes.borrow(), [3.0, 2.0], "the target, then the prior back");
    assert_eq!(venue.held.get(), 2.0);

    // A prior of 3x writes 4x instead, so the write is always a CHANGE the poll can see.
    let venue = Venue::holding(3.0, false);
    assert_eq!(
        leverage_write_read_back(&venue, "X", "DEMO, cross", "cross leverage", |x| venue.write(x)),
        Ok((3.0, 4.0))
    );
    assert_eq!(*venue.writes.borrow(), [4.0, 3.0]);
}

#[test]
fn an_open_position_is_a_skip_and_nothing_is_written() {
    let venue = Venue::holding(5.0, true);
    let answer =
        leverage_write_read_back(&venue, "BTCUSDT", "DEMO", "leverage", |x| venue.write(x));
    assert_eq!(answer, Err("the demo account holds an open BTCUSDT position".to_string()));
    assert!(venue.writes.borrow().is_empty());
}

/// A client that records each cancel it is asked for.
#[derive(Default)]
struct Cancels(Mutex<Vec<String>>);

impl VenueRest for Cancels {
    fn submit_order(&self, _request: &OrderRequest) -> Vec<Event> {
        Vec::new()
    }
    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        self.0.lock().expect("lock").push(client_order_id.to_string());
        Ok(())
    }
}

#[test]
fn the_cancel_guard_cancels_its_coid_once_when_dropped() {
    let rest = Cancels::default();
    {
        let _guard = CancelOnDrop { rest: &rest, coid: "c-1", host: "demo" };
        assert!(rest.0.lock().expect("lock").is_empty(), "nothing is cancelled while it is held");
    }
    assert_eq!(*rest.0.lock().expect("lock"), ["c-1"]);
}
