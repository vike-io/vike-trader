//! A scripted signed REST wire for a bridge's offline client tests (`test-support` only, the
//! `scripted` shape).
//!
//! The leverage readers of binance, bybit and okx are each tested over a wire that answers per path
//! from a queue and logs every call; the queue and the log are venue-neutral, so they are written here
//! once. Each venue implements its own transport trait over [`ScriptedRest`] (binance's is this crate's
//! `RestTransport`, implemented below) and keeps its own answers.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use serde_json::Value;

use crate::signer::Signer;
use crate::transport::{RestTransport, VenueApiError};

/// One call the client made: its method, path and the `(key, value)` pairs it sent.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub params: Vec<(String, String)>,
}

/// Answers per path from a queue (the last answer repeats) and logs every call. The log is shared, so a
/// test reads it after the client owns the wire.
pub struct ScriptedRest {
    pub log: Rc<RefCell<Vec<Call>>>,
    answers: RefCell<HashMap<&'static str, VecDeque<Result<Value, VenueApiError>>>>,
    unscripted: Option<VenueApiError>,
}

impl ScriptedRest {
    /// An empty script, and its log.
    pub fn new() -> (Self, Rc<RefCell<Vec<Call>>>) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let wire = ScriptedRest {
            log: Rc::clone(&log),
            answers: RefCell::new(HashMap::new()),
            unscripted: None,
        };
        (wire, log)
    }

    /// `answer` queued for `path`.
    pub fn then(self, path: &'static str, answer: Result<Value, VenueApiError>) -> Self {
        self.answers.borrow_mut().entry(path).or_default().push_back(answer);
        self
    }

    /// `body` queued for `path`.
    pub fn says(self, path: &'static str, body: Value) -> Self {
        self.then(path, Ok(body))
    }

    /// A path with nothing scripted answers `err`; without this the wire panics naming the path.
    pub fn unscripted(mut self, err: VenueApiError) -> Self {
        self.unscripted = Some(err);
        self
    }

    /// Log the call, then answer it from `path`'s queue.
    pub fn answer(
        &self,
        method: &str,
        path: &str,
        params: Vec<(String, String)>,
    ) -> Result<Value, VenueApiError> {
        self.log.borrow_mut().push(Call {
            method: method.to_string(),
            path: path.to_string(),
            params,
        });
        let mut answers = self.answers.borrow_mut();
        let Some(queue) = answers.get_mut(path) else {
            return Err(self.unscripted.clone().unwrap_or_else(|| {
                panic!("an unscripted read of {path}");
            }));
        };
        if queue.len() > 1 { queue.pop_front().expect("one") } else { queue[0].clone() }
    }
}

impl RestTransport for ScriptedRest {
    fn signed(
        &self,
        _base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        self.answer(
            method,
            path,
            params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect(),
        )
    }

    crate::no_public_call!("the reader made an unsigned read");
}
