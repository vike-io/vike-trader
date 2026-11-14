//! The three symbol-set verbs: the user's named symbol lists (the desktop's DataSets), held in this project's settings
//! database (`vike_secrets::symbol_set`, the owner's ruling of 2026-11-12). `SymbolSets` reads them (Observe);
//! `SetSymbolSet` and `DeleteSymbolSet` change ONE row by name (Control). Every answer is the whole list after the act,
//! so a client adopts the reply and never guesses what the server holds. They dial nothing and touch no store but that
//! one table.

use vike_data::datasets::SymbolSet;
use vike_datahub_client::proto::Response;
use vike_secrets::CredentialStore;
use vike_secrets::symbol_set::{
    SymbolSetRow, delete_symbol_set_row, read_symbol_set_rows, write_symbol_set_row,
};

/// The answer of a datahub mounted with no settings store: a test's server.
const NO_STORE: &str =
    "this datahub was started with no settings store, so it holds no symbol lists";

/// The longest name a list may carry, in bytes.
pub(crate) const SYMBOL_SET_MAX_NAME: usize = 128;
/// The most symbols one list may hold.
pub(crate) const SYMBOL_SET_MAX_SYMBOLS: usize = 2_000;
/// The longest symbol, provider, interval or benchmark word, in bytes.
pub(crate) const SYMBOL_SET_MAX_WORD: usize = 64;

/// Why a list from the wire is refused before it reaches the store, or `None`. Remote text, so its size is bounded
/// here, at the trust boundary.
fn refusal(set: &SymbolSet) -> Option<String> {
    let name = set.name.trim();
    if name.is_empty() || name.len() > SYMBOL_SET_MAX_NAME {
        return Some(format!("a symbol list needs a name of 1 to {SYMBOL_SET_MAX_NAME} bytes"));
    }
    if set.symbols.len() > SYMBOL_SET_MAX_SYMBOLS {
        return Some(format!("a symbol list holds at most {SYMBOL_SET_MAX_SYMBOLS} symbols"));
    }
    let mut words = set.symbols.iter().chain([&set.provider, &set.interval, &set.benchmark]);
    if let Some(long) = words.find(|w| w.len() > SYMBOL_SET_MAX_WORD) {
        return Some(format!("`{long:.32}…` is longer than {SYMBOL_SET_MAX_WORD} bytes"));
    }
    None
}

/// Every list the store holds, as the reply.
fn lists(dir: &std::path::Path) -> Response {
    match read_symbol_set_rows(dir) {
        Ok(rows) => Response::SymbolSets(
            rows.into_iter()
                .map(|r| SymbolSet {
                    name: r.name,
                    symbols: r.symbols,
                    provider: r.provider,
                    interval: r.interval,
                    benchmark: r.benchmark,
                    user: r.user,
                })
                .collect(),
        ),
        Err(e) => Response::Error(format!("the symbol lists could not be read: {e}")),
    }
}

/// The settings directory of `store`; `None` is answered with [`NO_STORE`].
fn settings_dir(store: Option<&CredentialStore>) -> Option<std::path::PathBuf> {
    store.and_then(CredentialStore::location).map(|l| l.settings_dir().to_path_buf())
}

fn no_store() -> Response {
    Response::Error(NO_STORE.to_string())
}

/// `Request::SymbolSets`.
pub(super) fn symbol_sets_verb(store: Option<&CredentialStore>) -> Response {
    settings_dir(store).map_or_else(no_store, |dir| lists(&dir))
}

/// `Request::SetSymbolSet`: one row replaced or added by name, then every list.
pub(super) fn set_symbol_set_verb(store: Option<&CredentialStore>, set: &SymbolSet) -> Response {
    let Some(dir) = settings_dir(store) else { return no_store() };
    if let Some(why) = refusal(set) {
        return Response::Error(format!("the symbol list was not saved: {why}"));
    }
    let row = SymbolSetRow {
        name: set.name.trim().to_string(),
        symbols: set.symbols.clone(),
        provider: set.provider.clone(),
        interval: set.interval.clone(),
        benchmark: set.benchmark.clone(),
        user: set.user,
    };
    match write_symbol_set_row(&dir, &row) {
        Ok(()) => lists(&dir),
        Err(e) => Response::Error(format!("the symbol list was not saved: {e}")),
    }
}

/// `Request::DeleteSymbolSet`: one row removed by name (a name no list has removes nothing), then every list.
pub(super) fn delete_symbol_set_verb(store: Option<&CredentialStore>, name: &str) -> Response {
    let Some(dir) = settings_dir(store) else { return no_store() };
    match delete_symbol_set_row(&dir, name) {
        Ok(_) => lists(&dir),
        Err(e) => Response::Error(format!("the symbol list was not removed: {e}")),
    }
}
