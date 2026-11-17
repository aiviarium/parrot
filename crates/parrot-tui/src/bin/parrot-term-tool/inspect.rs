//! `parrot-term-tool inspect`: a live split view. The program runs on the left as the emulator
//! sees it; its tree, state and surfaces are on the right. Keys go to the program; Ctrl+]
//! quits. The profile file is reloaded when it changes.

use parrot_tui::SessionExt;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use parrot_term::screen::{Attrs, CellKind, Color, Style};
use parrot_term::{PtyCommand, Screen, Session, SessionConfig, Size};
use parrot_tui::tree::{RenderOptions, render_with};
use parrot_tui::{Profile, TuiSnapshot};

use super::{RawMode, parse_size, terminal_size};

fn sgr(st: &Style) -> String {
    let mut p = vec!["0".to_string()];
    for (f, c) in [
        (Attrs::BOLD, "1"),
        (Attrs::DIM, "2"),
        (Attrs::ITALIC, "3"),
        (Attrs::UNDERLINE, "4"),
        (Attrs::REVERSE, "7"),
        (Attrs::STRIKETHROUGH, "9"),
    ] {
        if st.attrs.contains(f) {
            p.push(c.into());
        }
    }
    let col = |c: Color, base: u8| match c {
        Color::Default => None,
        Color::Indexed(i) => Some(format!("{};5;{i}", base)),
        Color::Rgb(r, g, b) => Some(format!("{};2;{r};{g};{b}", base)),
    };
    p.extend(col(st.fg, 38));
    p.extend(col(st.bg, 48));
    format!("\x1b[{}m", p.join(";"))
}

/// Draws the program's screen at the top left, `cols` wide.
fn draw_screen(out: &mut String, screen: &Screen, cols: u16, rows: u16) {
    for r in 0..rows {
        out.push_str(&format!("\x1b[{};1H\x1b[0m", r + 1));
        let Some(row) = screen.row(r) else {
            out.push_str(&" ".repeat(cols as usize));
            continue;
        };
        let mut last = Style::default();
        let mut c = 0;
        while c < cols.min(row.cols()) {
            let cell = row.cell(c);
            if cell.style() != last {
                last = cell.style();
                out.push_str(&sgr(&last));
            }
            match cell.kind() {
                CellKind::WideTail => {}
                CellKind::WrapSpacer => out.push(' '),
                CellKind::Wide if c + 1 >= cols => out.push(' '),
                _ => out.push_str(if cell.text().is_empty() { " " } else { cell.text() }),
            }
            c += 1;
        }
        out.push_str("\x1b[0m");
        for _ in c..cols {
            out.push(' ');
        }
    }
}

fn clip(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > width {
            break;
        }
        w += cw;
        out.push(ch);
    }
    out.push_str(&" ".repeat(width.saturating_sub(w)));
    out
}

fn draw_side(out: &mut String, snap: &TuiSnapshot, col: u16, width: u16, rows: u16, status: &str) {
    let mut lines = vec![format!(
        "state {} · holding [{}]{}",
        snap.state.as_deref().unwrap_or("none"),
        snap.states.join(", "),
        if snap.unrecognised { " · UNRECOGNISED" } else { "" }
    )];
    for s in &snap.surfaces {
        lines.push(format!(
            "surface {:?} {:?} options {} selected {:?}",
            s.kind,
            s.title.as_deref().unwrap_or(""),
            s.options.len(),
            s.selected
        ));
    }
    if let Some(t) = &snap.tree {
        let opts = RenderOptions {
            text_width: 40,
            sources: true,
            styles: false,
        };
        lines.extend(render_with(t, opts).lines().map(str::to_string));
    }
    for r in 0..rows {
        out.push_str(&format!("\x1b[{};{}H\x1b[0m│", r + 1, col));
        let l = lines.get(r as usize).map(String::as_str).unwrap_or("");
        if r == 0 {
            out.push_str("\x1b[1m");
        }
        out.push_str(&clip(l, width.saturating_sub(1) as usize));
        out.push_str("\x1b[0m");
    }
    out.push_str(&format!("\x1b[{};1H\x1b[7m", rows + 1));
    let total = (col as usize - 1) + width as usize;
    out.push_str(&clip(status, total));
    out.push_str("\x1b[0m");
}

fn mtime(p: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

pub fn run(args: &[String]) -> Result<ExitCode, String> {
    let mut profile_path: Option<PathBuf> = None;
    let mut size: Option<Size> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--profile" => {
                profile_path = Some(args.get(i + 1).ok_or("--profile FILE")?.into());
                i += 2;
            }
            "--size" => {
                size = Some(parse_size(args.get(i + 1).ok_or("--size COLSxROWS")?)?);
                i += 2;
            }
            "--" => {
                i += 1;
                break;
            }
            _ => break,
        }
    }
    let cmd_args = &args[i..];
    let (program, rest) = cmd_args.split_first().ok_or("inspect -- CMD [ARGS...]")?;
    let term = terminal_size().unwrap_or(Size::new(40, 160));
    let rows = term.rows.saturating_sub(1).max(5);
    let size = size.unwrap_or(Size::new(rows, (term.cols * 11 / 20).max(20)));
    let side_col = size.cols + 1;
    let side_width = term.cols.saturating_sub(size.cols);
    let load = |p: &PathBuf| Profile::load(p).map_err(|e| e.to_string());
    let mut profile = match &profile_path {
        Some(p) => Some(load(p)?),
        None => None,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async move {
        let cmd = PtyCommand::new(program.clone())
            .args(rest.iter().cloned())
            .env("TERM", "xterm-256color");
        let (session, pty) = Session::spawn(&cmd, SessionConfig::for_automation(size))
            .map_err(|e| format!("{program}: {e}"))?;
        let tui = session.automate(profile.take());
        let raw = RawMode::enable();
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x1b[?1049h\x1b[?25l\x1b[2J");
        let _ = stdout.flush();
        let (in_tx, mut in_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 || in_tx.blocking_send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut seen = profile_path.as_ref().and_then(mtime);
        let mut status = format!(
            "{} · profile {} · Ctrl+] quits",
            program,
            profile_path
                .as_ref()
                .map_or("none".to_string(), |p| p.display().to_string())
        );
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut reload = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = pty.wait() => break,
                Some(data) = in_rx.recv() => {
                    if data.contains(&0x1d) {
                        break;
                    }
                    let _ = session.write(&data).await;
                }
                _ = reload.tick() => {
                    if let Some(p) = &profile_path {
                        let m = mtime(p);
                        if m != seen {
                            seen = m;
                            match load(p) {
                                Ok(pr) => {
                                    tui.reload(Some(pr));
                                    status = format!("{program} · reloaded {} · Ctrl+] quits", p.display());
                                }
                                Err(e) => status = format!("profile error: {e}"),
                            }
                        }
                    }
                }
                _ = tick.tick() => {
                    let mut out = String::new();
                    draw_screen(&mut out, &session.screen(), size.cols, size.rows.min(rows));
                    draw_side(&mut out, &tui.snapshot(), side_col, side_width, rows, &status);
                    let _ = stdout.write_all(out.as_bytes());
                    let _ = stdout.flush();
                }
            }
        }
        let _ = pty.kill();
        let _ = stdout.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = stdout.flush();
        drop(raw);
        Ok(ExitCode::SUCCESS)
    })
}
