//! Extension startup must never become the terminal input deadline.
//!
//! The window between the first editable frame and the resolved theme is the
//! extension attach round: it waits on extension processes and their
//! provider-registration barrier, and it is the round that finally contributes
//! the theme. Nothing in that window may hold the only terminal input owner, so
//! the pump contract below is a regression, not an optimisation.

use super::*;

#[tokio::test]
async fn pending_extension_attach_round_keeps_terminal_input_live() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let mut shell = InteractiveShell::test_shell();
    // Two ordinary keys, then a stream that stays open and silent. The counter
    // records how often the pump came back to the only terminal input owner.
    let polls = Arc::new(AtomicUsize::new(0));
    let counting = polls.clone();
    let mut input = futures_util::stream::poll_fn(
        move |_| -> std::task::Poll<Option<std::io::Result<Event>>> {
            let index = counting.fetch_add(1, Ordering::SeqCst);
            let code = match index {
                0 => KeyCode::Char('a'),
                1 => KeyCode::Char('b'),
                _ => return std::task::Poll::Pending,
            };
            std::task::Poll::Ready(Some(Ok(Event::Key(KeyEvent::new(
                code,
                KeyModifiers::NONE,
            )))))
        },
    );
    // The awaited round stands in for extension startup: it settles only after
    // the pump has read past both keys, so a pump that waited for the round
    // first could never produce its value. The round keeps the deadline it has
    // in production, which is none.
    let witnessed = polls.clone();
    let attach = async move {
        while witnessed.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        "attached"
    };
    let settled = tokio::time::timeout(
        Duration::from_secs(5),
        pump_owned(&mut shell, &mut input, attach),
    )
    .await
    .expect("a pending extension round must not hold terminal input")
    .expect("the pump returns the owned work");

    assert_eq!(settled, "attached");
    // Both keys reached the draft while the round was still outstanding.
    assert_eq!(shell.pending(), "ab");
    assert!(!shell.close_requested());
}

#[tokio::test]
async fn a_pending_extension_round_still_settles_after_a_close_request() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let mut shell = InteractiveShell::test_shell();
    let keys: Vec<std::io::Result<Event>> = vec![Ok(Event::Key(KeyEvent::new(
        KeyCode::Char('d'),
        KeyModifiers::CONTROL,
    )))];
    let mut input = tokio_stream::iter(keys);
    // The round keeps owning the application across the close request: the pump
    // returns it instead of detaching it, so shutdown never loses the fleet.
    let finished = Arc::new(AtomicBool::new(false));
    let owned = finished.clone();
    let attach = async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        owned.store(true, Ordering::SeqCst);
        "attached"
    };
    let settled = tokio::time::timeout(
        Duration::from_secs(5),
        pump_owned(&mut shell, &mut input, attach),
    )
    .await
    .expect("a close request must not hold the owned work open")
    .expect("the pump returns the owned work");

    assert_eq!(settled, "attached");
    assert!(finished.load(Ordering::SeqCst));
    assert!(shell.close_requested());
}
