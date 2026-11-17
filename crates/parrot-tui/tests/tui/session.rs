use parrot_term::{Session, SessionConfig, Size};
use std::time::Duration;

use parrot_term::io::channel;
use parrot_tui::{Locator, SessionExt};
use regex::Regex;

const T: Duration = Duration::from_secs(10);

fn config(rows: u16, cols: u16) -> SessionConfig {
    let mut c = SessionConfig::with_size(Size::new(rows, cols));
    c.settle.quiet = Duration::from_millis(30);
    c
}

#[tokio::test]
async fn waits_time_out_with_the_screen() {
    let (source, sink, out, _inp) = channel();
    let session = Session::new(source, sink, config(3, 20));
    out.send(b"nothing here".to_vec()).await.unwrap();
    let err = session
        .wait_for_text("never", Duration::from_millis(100))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("timed out"));
    assert!(err.to_string().contains("0│nothing here"));
    session
        .wait_for_stable(Duration::from_millis(50), T)
        .await
        .unwrap();
    let m = session
        .wait_for_locator(&Locator::regex(Regex::new(r"\bh[a-z]+").unwrap()), T)
        .await
        .unwrap();
    assert_eq!((m.col, m.text.as_str()), (8, "here"));
}
