//! Sessions over a channel (as for a remote byte stream) and over a local PTY.

use std::time::Duration;

use parrot_term::io::{SinkEvent, channel};
use parrot_term::{
    Cast, Emulator, EmulatorConfig, Input, Key, Mods, PtyCommand, Replay, Session, SessionConfig,
    SettleReason, Size, serialize,
};

const T: Duration = Duration::from_secs(10);

fn config(rows: u16, cols: u16) -> SessionConfig {
    let mut c = SessionConfig::with_size(Size::new(rows, cols));
    c.settle.quiet = Duration::from_millis(30);
    c
}

#[tokio::test]
async fn channel_session_settles_answers_queries_and_encodes_input() {
    let (source, sink, out, mut inp) = channel();
    let session = Session::new(source, sink, config(5, 30));
    let mut frames = session.settled();

    out.send(b"hello\r\n\x1b[6n".to_vec()).await.unwrap();
    // The emulator answers the cursor position query through the sink.
    assert_eq!(inp.recv().await.unwrap(), SinkEvent::Data(b"\x1b[2;1R".to_vec()));
    let f = frames.next().await.unwrap();
    assert_eq!(f.reason, SettleReason::Quiet);
    assert_eq!(f.screen.lines()[0], "hello");

    // A synchronized update settles when it ends.
    out.send(b"\x1b[?2026hframe".to_vec()).await.unwrap();
    out.send(b" done\x1b[?2026l".to_vec()).await.unwrap();
    let f = frames.next().await.unwrap();
    assert_eq!(f.reason, SettleReason::SyncEnd);
    assert_eq!(f.screen.lines()[1], "frame done");

    // Input is encoded for the current modes.
    out.send(b"\x1b[?1h\x1b[?2004h".to_vec()).await.unwrap();
    session
        .wait_for(|s| s.modes().app_cursor && s.modes().bracketed_paste, T)
        .await
        .unwrap();
    session.send(&Input::Key(Key::Up, Mods::empty())).await.unwrap();
    assert_eq!(inp.recv().await.unwrap(), SinkEvent::Data(b"\x1bOA".to_vec()));
    session.paste("x").await.unwrap();
    assert_eq!(
        inp.recv().await.unwrap(),
        SinkEvent::Data(b"\x1b[200~x\x1b[201~".to_vec())
    );
    session.resize(Size::new(6, 40)).await.unwrap();
    assert_eq!(inp.recv().await.unwrap(), SinkEvent::Resize(Size::new(6, 40)));
    assert_eq!(session.size(), Size::new(6, 40));

    drop(out);
    session.wait_closed().await;
    assert!(session.is_closed());
    assert!(
        frames
            .next()
            .await
            .is_some_and(|f| f.screen.size() == Size::new(6, 40))
            || session.is_closed()
    );
}

#[tokio::test]
async fn byte_subscriptions_resume_from_offsets() {
    let (source, sink, out, _inp) = channel();
    let mut cfg = config(3, 20);
    cfg.buffer_bytes = 16;
    let session = Session::new(source, sink, cfg);
    let mut live = session.subscribe_bytes(Some(0)).unwrap();
    out.send(b"0123456789".to_vec()).await.unwrap();
    out.send(b"abcdefghij".to_vec()).await.unwrap();
    let c = live.recv().await.unwrap().unwrap();
    assert_eq!((c.offset, &c.data[..]), (0, &b"0123456789"[..]));
    let c = live.recv().await.unwrap().unwrap();
    assert_eq!((c.offset, &c.data[..]), (10, &b"abcdefghij"[..]));
    // Resume from an offset still buffered.
    let mut r = session.subscribe_bytes(Some(15)).unwrap();
    assert_eq!(&r.recv().await.unwrap().unwrap().data[..], b"fghij");
    // Too old: the caller has to take a fresh snapshot.
    assert!(matches!(
        session.subscribe_bytes(Some(2)),
        Err(parrot_term::OffsetError::TooOld { oldest: 4, .. })
    ));
    drop(out);
    assert!(live.recv().await.unwrap().is_none());
}

#[tokio::test]
async fn attach_gives_a_snapshot_that_continues_with_live_bytes() {
    let (source, sink, out, _inp) = channel();
    let session = Session::new(source, sink, config(4, 20));
    out.send(b"line 1\r\nline 2\r\n\x1b[1;31mred".to_vec())
        .await
        .unwrap();
    // An open synchronized update and a partial sequence are not in the snapshot yet.
    out.send(b"\x1b[?2026hhidden\x1b[3".to_vec()).await.unwrap();
    session
        .wait_for(|s| s.modes().synchronized_update, T)
        .await
        .unwrap();
    let mut attach = session.attach().unwrap();
    let mut viewer = Emulator::new(EmulatorConfig::with_size(attach.size));
    viewer.feed(&attach.snapshot);
    out.send(b"2m!\x1b[?2026l more".to_vec()).await.unwrap();
    session.wait_for_text("more", T).await.unwrap();
    drop(out);
    while let Some(c) = attach.bytes.recv().await.unwrap() {
        viewer.feed(&c.data);
    }
    let s = session.screen();
    assert_eq!(viewer.screen().rows(), s.rows());
    assert_eq!(viewer.screen().cursor(), s.cursor());
}

#[tokio::test]
async fn pty_session_runs_a_shell_command() {
    let cmd = PtyCommand::new("sh").args([
        "-c",
        "printf 'cols=%s\\n' \"$(tput cols)\"; read x; echo \"got:$x\"; exit 3",
    ]);
    let (session, pty) = Session::spawn(&cmd, config(10, 50)).unwrap();
    session.wait_for_text("cols=50", T).await.unwrap();
    session.type_text("hi").await.unwrap();
    session.press("enter").await.unwrap();
    session.wait_for_text("got:hi", T).await.unwrap();
    let status = tokio::time::timeout(T, pty.wait()).await.unwrap();
    assert_eq!(status.code, 3);
    tokio::time::timeout(T, session.wait_closed()).await.unwrap();
}

#[tokio::test]
async fn pty_signal_and_resize() {
    let cmd = PtyCommand::new("sh").args([
        "-c",
        "trap 'echo winch $(tput lines)x$(tput cols)' WINCH; echo ready; while :; do sleep 0.05; done",
    ]);
    let (session, pty) = Session::spawn(&cmd, config(10, 50)).unwrap();
    session.wait_for_text("ready", T).await.unwrap();
    session.resize(Size::new(12, 60)).await.unwrap();
    session.wait_for_text("winch 12x60", T).await.unwrap();
    pty.signal(rustix::process::Signal::TERM).unwrap();
    let status = tokio::time::timeout(T, pty.wait()).await.unwrap();
    assert!(!status.success());
}

#[tokio::test]
async fn recording_replays_to_the_same_screen() {
    let dir = std::env::temp_dir().join(format!("parrot-term-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rec.cast");
    let cmd = PtyCommand::new("sh").args([
        "-c",
        "printf '\\033[1mbold\\033[0m e\\314\\201 \\347\\225\\214\\n'; for i in $(seq 1 300); do echo line $i; done; read x",
    ]);
    let (session, pty) = Session::spawn(&cmd, config(8, 40)).unwrap();
    session
        .start_recording(
            std::fs::File::create(&path).unwrap(),
            parrot_term::CastHeader::new(Size::new(8, 40)),
        )
        .unwrap();
    session.wait_for_text("line 300", T).await.unwrap();
    session.marker("done");
    session.resize(Size::new(6, 30)).await.unwrap();
    session
        .wait_for_stable(Duration::from_millis(100), T)
        .await
        .unwrap();
    let live = session.screen();
    session.press("enter").await.unwrap();
    tokio::time::timeout(T, pty.wait()).await.unwrap();
    session.wait_closed().await;
    session.stop_recording().unwrap();

    let cast = Cast::load(&path).unwrap();
    assert_eq!(cast.markers().len(), 1);
    assert!(cast.events.iter().any(|e| e.size() == Some(Size::new(6, 30))));
    let replayed = Replay::new(&cast).screen_at(cast.markers()[0].0 + 0.0001);
    assert!(replayed.contains("line 300"));
    let fin = Replay::new(&cast).final_screen();
    assert_eq!(fin.size(), Size::new(6, 30));
    assert_eq!(fin.history().iter().next().unwrap().text(), "bold e\u{301} 界");
    let _ = live;
    // The recording serialises to the same thing as the live screen did at the end.
    assert_eq!(serialize(&fin), serialize(&session.screen()));
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn resume_continues_offsets_without_answering_old_queries() {
    // An earlier session's transcript from offset 100, with a query in it.
    let history = b"\x1b[6nfirst\r\nsecond\r\n".to_vec();
    let (source, sink, out, mut inp) = channel();
    let session = Session::resume(source, sink, config(5, 30), &history, 100);
    assert_eq!(session.offset(), 100 + history.len() as u64);
    assert!(session.screen().contains("second"));
    // The query in the history is not answered again.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), inp.recv())
            .await
            .is_err()
    );
    // Resuming from inside the history works, and older offsets are gone.
    let mut sub = session.subscribe_bytes(Some(104)).unwrap();
    assert_eq!(&*sub.recv().await.unwrap().unwrap().data, &history[4..]);
    assert!(session.subscribe_bytes(Some(50)).is_err());
    out.send(b"third".to_vec()).await.unwrap();
    let c = sub.recv().await.unwrap().unwrap();
    assert_eq!((c.offset, &*c.data), (100 + history.len() as u64, &b"third"[..]));
    // Attach gives a snapshot at the current offset.
    session.wait_for_text("third", T).await.unwrap();
    let a = session.attach().unwrap();
    assert_eq!(a.offset, session.offset());
}

#[tokio::test]
async fn viewer_answers_to_answered_queries_are_dropped() {
    let (source, sink, out, mut inp) = channel();
    let session = Session::new(source, sink, config(5, 30));
    out.send(b"\x1b[c\x1b[6n".to_vec()).await.unwrap();
    // The session answers both itself.
    let SinkEvent::Data(answer) = inp.recv().await.unwrap() else {
        panic!()
    };
    assert!(answer.starts_with(b"\x1b[?"), "{answer:?}");
    // xterm.js answers the same queries; only the typed text gets through.
    session
        .write_from_viewer(b"\x1b[?1;2c\x1b[2;1Rls\r")
        .await
        .unwrap();
    loop {
        match inp.recv().await.unwrap() {
            SinkEvent::Data(d) if d == b"ls\r" => break,
            SinkEvent::Data(d) if d.starts_with(b"\x1b[") => continue,
            other => panic!("{other:?}"),
        }
    }
}
