//! A scripted TUI for the driver tests, drawing the same screens on every OS: a prompt, a busy
//! line, a tool approval, a trust dialog at start, and a question no profile knows. With
//! `FAKE_TUI_TRANSCRIPT` set it writes a Claude-style transcript there.

use std::io::{BufRead, Write};
use std::time::Duration;

const SESSION: &str = "fake-session-0001";
const RULE: &str = "────────────────────────────────────────";

struct Fake {
    /// The folder the transcript goes in, when the test asks for one.
    transcript: Option<String>,
    out: std::io::Stdout,
    /// Prompts seen, for the transcript's ids.
    n: u32,
}

impl Fake {
    fn write(&mut self, text: &str) {
        let _ = self.out.write_all(text.as_bytes());
        let _ = self.out.flush();
    }

    fn clear(&mut self) {
        self.write("\x1b[2J\x1b[H");
    }

    fn idle(&mut self, line: &str) {
        self.clear();
        self.write(&format!(
            "Fake TUI 1.0\r\n{line}\r\n\r\n? Type a prompt and press Enter\x1b[3;1H> "
        ));
    }

    fn busy(&mut self, secs: f64) {
        self.clear();
        self.write("Fake TUI 1.0\r\n\r\n* Working… (esc to interrupt)");
        std::thread::sleep(Duration::from_secs_f64(secs));
    }

    fn menu(&mut self, line: &str) {
        self.clear();
        self.write(&format!(
            "Fake TUI 1.0\r\n{RULE}\r\n{line}\r\n ❯ 1. Yes\r\n   2. No\r\n\r\n Esc to cancel\r\n"
        ));
    }

    /// Appends one line to the transcript, as Claude Code writes it.
    fn log(&self, line: String) {
        let Some(dir) = &self.transcript else {
            return;
        };
        let path = std::path::Path::new(dir).join(format!("{SESSION}.jsonl"));
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| writeln!(f, "{line}"));
    }
}

fn main() {
    let mut fake = Fake {
        transcript: std::env::var("FAKE_TUI_TRANSCRIPT")
            .ok()
            .filter(|s| !s.is_empty()),
        out: std::io::stdout(),
        n: 0,
    };
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();

    if std::env::var_os("FAKE_TUI_TRUST").is_some() {
        fake.menu("Do you trust the files in this folder?");
        match lines.next() {
            Some(Ok(answer)) if answer.trim() == "1" => {}
            _ => std::process::exit(1),
        }
    }

    fake.idle("Ready.");
    while let Some(Ok(line)) = lines.next() {
        let line = line.trim_end().to_string();
        fake.n += 1;
        if line.starts_with("approve") {
            fake.busy(0.3);
            fake.menu("Do you want to run fake-tool?");
            let allowed = matches!(lines.next(), Some(Ok(a)) if a.trim() == "1");
            fake.busy(0.3);
            fake.idle(if allowed { "Tool allowed." } else { "Tool denied." });
        } else if line.starts_with("mystery") {
            fake.busy(0.3);
            fake.clear();
            fake.write("Fake TUI 1.0\r\n\r\nPick a colour: red, green or blue\r\ncolour: ");
            let colour = match lines.next() {
                Some(Ok(c)) => c.trim_end().to_string(),
                _ => break,
            };
            fake.busy(0.3);
            fake.idle(&format!("You picked {colour}."));
        } else {
            fake.log(format!(
                "{{\"type\":\"user\",\"uuid\":\"fake-u-{}\",\"sessionId\":\"{SESSION}\",\"message\":{{\"role\":\"user\",\"content\":\"{line}\"}}}}",
                fake.n
            ));
            fake.busy(0.5);
            fake.log(format!(
                "{{\"type\":\"assistant\",\"uuid\":\"fake-a-{}\",\"sessionId\":\"{SESSION}\",\"message\":{{\"id\":\"msg_fake_{}\",\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"Hi from the fake.\"}}],\"stop_reason\":\"end_turn\"}}}}",
                fake.n, fake.n
            ));
            fake.idle(&format!("Done: {line}"));
        }
    }
}
