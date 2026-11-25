use super::*;

/// `retry_on_pauses`: a settled first answer waits for nothing; each miss waits the NEXT pause, in
/// order, and asks again with the next attempt number; the first settled answer wins; with every
/// pause spent the LAST answer stands, never slept after. Red on a mutant that sleeps before the
/// first ask or after the last, asks once too often, or numbers the attempts from 0.
#[test]
fn a_ladder_waits_each_pause_in_turn_and_the_last_answer_stands() {
    let ms = Duration::from_millis;
    let pauses = [ms(1), ms(2), ms(3)];
    let run = |settle_at: u32| {
        let (mut asked, mut waits) = (Vec::new(), Vec::new());
        let got = retry_on_pauses(
            &pauses,
            |attempt| {
                asked.push(attempt);
                attempt
            },
            |answer| *answer >= settle_at,
            |pause| waits.push(pause),
        );
        (got, asked, waits)
    };
    assert_eq!(run(1), (1, vec![1], vec![]));
    assert_eq!(run(3), (3, vec![1, 2, 3], vec![ms(1), ms(2)]));
    assert_eq!(run(9), (4, vec![1, 2, 3, 4], pauses.to_vec()), "spent: the last answer stands");
    let mut asks = 0;
    retry_on_pauses(&[], |_| asks += 1, |_| false, |_| panic!("no pause"));
    assert_eq!(asks, 1, "no pause is one ask");
}
