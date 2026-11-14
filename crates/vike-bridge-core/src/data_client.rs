//! **The market-data clients a bridge is built from.** [`ws`] is the client of a venue whose every
//! subscription is its own text WebSocket (`DataClientWebsocket` over a venue's `DataClientSpec`); [`bus`] is
//! the client over a SHARED source, where one connection or one process-wide source carries every
//! symbol (`DataClientBus` over a venue's `DataClientBusSpec`). [`context`] is one feed thread's
//! context and [`event`] what one decoded frame says, the vocabulary every data client shares.

pub mod bus;
pub mod context;
pub mod event;
pub mod ws;
