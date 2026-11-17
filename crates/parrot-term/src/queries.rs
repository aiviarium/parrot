//! Answers to terminal queries in a viewer's input.
//!
//! A [`crate::Session`] answers the application's queries itself (device attributes, cursor position,
//! colours, modes). A viewer that renders the same output, such as xterm.js, answers them too, and its
//! answers arrive as input. [`ResponseFilter`] drops them, so the application sees each answer once.
//!
//! Most answers have a shape no key produces (`CSI ? 1 ; 2 c`, `OSC 11 ; rgb:… ST`, `DCS > | … ST`) and are
//! always dropped. A cursor position report (`CSI row ; col R`) looks like a modified F3 key in some
//! encodings, so it is dropped only within [`CPR_WINDOW`] of a query the session saw.

use std::time::{Duration, Instant};

/// How long after a query a cursor position report counts as an answer.
pub const CPR_WINDOW: Duration = Duration::from_secs(2);

#[derive(Debug, Default, Clone)]
pub struct ResponseFilter {
    last_query: Option<Instant>,
}

impl ResponseFilter {
    /// The session just answered a query.
    pub fn note_query(&mut self) {
        self.last_query = Some(Instant::now());
    }

    fn cpr_expected(&self) -> bool {
        self.last_query.is_some_and(|t| t.elapsed() < CPR_WINDOW)
    }

    /// The input without answers to queries.
    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        let cpr = self.cpr_expected();
        let mut out = Vec::with_capacity(input.len());
        let mut i = 0;
        while i < input.len() {
            if input[i] == 0x1b
                && let Some(len) = response_len(&input[i..], cpr)
            {
                i += len;
                continue;
            }
            out.push(input[i]);
            i += 1;
        }
        out
    }
}

/// The length of the answer at the start of `s`, if one is there.
fn response_len(s: &[u8], cpr: bool) -> Option<usize> {
    match s.get(1)? {
        b'[' => csi_response(s, cpr),
        b']' => osc_response(s),
        b'P' => dcs_response(s),
        _ => None,
    }
}

fn csi_response(s: &[u8], cpr: bool) -> Option<usize> {
    // ESC [ <private?> <params> <intermediate?> <final>
    let mut i = 2;
    let private = match s.get(i) {
        Some(c @ (b'?' | b'>' | b'=')) => {
            i += 1;
            Some(*c)
        }
        _ => None,
    };
    let params_start = i;
    while i < s.len() && (s[i].is_ascii_digit() || s[i] == b';') {
        i += 1;
    }
    let params = &s[params_start..i];
    let intermediate = match s.get(i) {
        Some(b'$') => {
            i += 1;
            Some(b'$')
        }
        _ => None,
    };
    let fin = *s.get(i)?;
    let len = i + 1;
    let semis = params.iter().filter(|c| **c == b';').count();
    let drop = match (private, intermediate, fin) {
        // Device attributes: primary (CSI ? … c), secondary (CSI > … c), tertiary (CSI = … c).
        (Some(_), None, b'c') => !params.is_empty(),
        // Mode reports (DECRPM): CSI ? Ps ; Pm $ y and CSI Ps ; Pm $ y.
        (_, Some(b'$'), b'y') => semis == 1,
        // Kitty keyboard flags: CSI ? flags u.
        (Some(b'?'), None, b'u') => true,
        // Extended cursor position (DECXCPR): CSI ? row ; col R.
        (Some(b'?'), None, b'R') => semis >= 1,
        // Status report: CSI 0 n (ok) or CSI 3 n.
        (None, None, b'n') => params == b"0" || params == b"3",
        // Cursor position report, only while one is expected.
        (None, None, b'R') => cpr && semis == 1,
        // Window size reports: CSI 4 ; h ; w t, CSI 8 ; rows ; cols t.
        (None, None, b't') => semis == 2 && (params.starts_with(b"4;") || params.starts_with(b"8;")),
        _ => false,
    };
    drop.then_some(len)
}

/// The length up to and including the string terminator (BEL or ESC \), if there is one.
fn until_st(s: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < s.len() {
        match s[i] {
            0x07 => return Some(i + 1),
            0x1b if s.get(i + 1) == Some(&b'\\') => return Some(i + 2),
            _ => i += 1,
        }
    }
    None
}

fn osc_response(s: &[u8]) -> Option<usize> {
    // Colour answers: OSC 4 ; n ; rgb:… , OSC 10/11/12 ; rgb:… . A key never produces OSC.
    let body = &s[2..];
    let colour = body.starts_with(b"10;")
        || body.starts_with(b"11;")
        || body.starts_with(b"12;")
        || body.starts_with(b"4;");
    if !colour {
        return None;
    }
    let len = until_st(s, 2)?;
    let content = &s[2..len];
    content.windows(4).any(|w| w == b"rgb:").then_some(len)
}

fn dcs_response(s: &[u8]) -> Option<usize> {
    // XTVERSION (DCS > | text ST), DECRQSS (DCS 0/1 $ r … ST), terminfo (DCS 0/1 + r … ST), DA3 (DCS ! | … ST).
    let body = &s[2..];
    let known = body.starts_with(b">|")
        || body.starts_with(b"!|")
        || body.starts_with(b"1$r")
        || body.starts_with(b"0$r")
        || body.starts_with(b"1+r")
        || body.starts_with(b"0+r");
    if !known {
        return None;
    }
    until_st(s, 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(input: &[u8]) -> Vec<u8> {
        ResponseFilter::default().filter(input)
    }

    #[test]
    fn drops_answers_no_key_produces() {
        assert_eq!(f(b"a\x1b[?1;2cb"), b"ab");
        assert_eq!(f(b"\x1b[>0;276;0c"), b"");
        assert_eq!(f(b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\x"), b"x");
        assert_eq!(f(b"\x1b]10;rgb:ffff/ffff/ffff\x07"), b"");
        assert_eq!(f(b"\x1bP>|xterm.js(5.5.0)\x1b\\"), b"");
        assert_eq!(f(b"\x1b[?2004;2$y"), b"");
        assert_eq!(f(b"\x1b[?0u"), b"");
        assert_eq!(f(b"\x1b[0n"), b"");
        assert_eq!(f(b"\x1bP1$r0m\x1b\\"), b"");
    }

    #[test]
    fn keeps_keys() {
        for key in [
            &b"\x1b[A"[..],
            b"\x1b[1;5D",
            b"\x1b[3~",
            b"\x1bOR",
            b"\x1b[200~paste\x1b[201~",
            b"\x1b[I",
            b"\x1b",
            b"\x1b[<0;10;5M",
            b"\x1b]52;c;aGk=\x07",
        ] {
            assert_eq!(f(key), key, "{key:?}");
        }
    }

    #[test]
    fn cursor_reports_only_after_a_query() {
        let mut r = ResponseFilter::default();
        // Without a query, CSI 1;5R is Ctrl+F3 in xterm's encoding and stays.
        assert_eq!(r.filter(b"\x1b[1;5R"), b"\x1b[1;5R");
        r.note_query();
        assert_eq!(r.filter(b"\x1b[12;40Rls"), b"ls");
    }

    #[test]
    fn incomplete_sequences_pass() {
        assert_eq!(f(b"\x1b]11;rgb:00"), b"\x1b]11;rgb:00");
        assert_eq!(f(b"\x1b[?1;2"), b"\x1b[?1;2");
    }
}
