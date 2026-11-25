//! Unit tests of the editor's pure helpers.

use super::*;

/// An account-row act's answer is coloured by whether the store REFUSED it (`Err`), never by its words: a write the
/// store made reads configured whatever it says, and a refusal reads as an error whatever it says.
#[test]
fn an_account_row_answer_is_coloured_by_its_outcome_and_not_its_words() {
    let refused = Err("account row 3 still owns credentials: venue_key".to_string());
    assert_eq!(
        row_message_words(&refused),
        ("account row 3 still owns credentials: venue_key", ERROR_COLOR)
    );
    let done = Ok("account row 3: unchanged — NOTHING WAS WRITTEN".to_string());
    assert_eq!(row_message_words(&done).1, CONFIGURED_COLOR);
}
