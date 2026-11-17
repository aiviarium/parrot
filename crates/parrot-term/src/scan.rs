//! A small scanner over terminal output that finds escape sequence boundaries.
//!
//! The emulator uses it to:
//! - hold back an incomplete escape sequence or UTF-8 character at the end of a chunk, so the
//!   parser is always between sequences when a snapshot is taken and the snapshot's byte offset
//!   is a clean place to resume a viewer;
//! - handle synchronized updates (mode 2026) itself;
//! - split the stream right before sequences whose effect it has to observe from outside the
//!   parser (switching to the alternate screen, clearing the scrollback, full reset, setting the
//!   scrolling region).

/// Sequences the scanner reports separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// `CSI ? 2026 h`
    SyncBegin,
    /// `CSI ? 2026 l`
    SyncEnd,
    /// `CSI ? 1049 h`, `CSI ? 1047 h` or `CSI ? 47 h`
    AltEnter,
    /// `CSI 3 J`
    ClearHistory,
    /// `ESC c`
    Reset,
    /// `CSI Pt ; Pb r`, with the raw parameters.
    ScrollRegion(Option<u16>, Option<u16>),
}

pub(crate) enum Item<'a> {
    /// Bytes to pass to the parser.
    Data(&'a [u8]),
    /// A complete sequence of interest.
    Seq(&'a [u8], Class),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    Utf8(u8),
    Esc,
    EscInt,
    Csi,
    Str,
    StrEsc,
}

/// Longest sequence held back; longer ones (large OSC strings) are passed through as they come.
const HOLD_LIMIT: usize = 4096;
const CSI_LIMIT: usize = 64;

#[derive(Default)]
pub(crate) struct Scanner {
    state: State,
    /// Bytes of the current incomplete sequence from earlier calls.
    held: Vec<u8>,
    /// The current sequence is too long and is being passed through.
    overflow: bool,
    /// Parameter, intermediate and final bytes of the current CSI.
    csi: Vec<u8>,
}

impl Scanner {
    /// Bytes held back because they are an incomplete sequence.
    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// Scans `input`, calling `out` with pieces in order. Data pieces end at a sequence boundary
    /// unless a sequence is longer than the hold limit.
    pub fn scan(&mut self, input: &[u8], out: &mut dyn FnMut(Item<'_>)) {
        // Index in `input` where unreported bytes begin.
        let mut pass_from = 0usize;
        // Index in `input` where the current sequence began, if it began in this call and is
        // being held.
        let mut seq_start: Option<usize> = None;

        let mut i = 0usize;
        while i < input.len() {
            let b = input[i];
            let mut complete: Option<Option<Class>> = None;
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.state = State::Esc;
                        seq_start = Some(i);
                    } else if (0xc2..=0xf4).contains(&b) {
                        let need = if b < 0xe0 {
                            1
                        } else if b < 0xf0 {
                            2
                        } else {
                            3
                        };
                        self.state = State::Utf8(need);
                        seq_start = Some(i);
                    }
                }
                State::Utf8(n) => {
                    if (0x80..=0xbf).contains(&b) {
                        if n == 1 {
                            complete = Some(None);
                        } else {
                            self.state = State::Utf8(n - 1);
                        }
                    } else {
                        // Invalid: end the character here and look at this byte again.
                        self.finish_seq(input, i, &mut pass_from, &mut seq_start, None, out);
                        continue;
                    }
                }
                State::Esc => match b {
                    b'[' => {
                        self.state = State::Csi;
                        self.csi.clear();
                    }
                    b']' | b'P' | b'X' | b'^' | b'_' => self.state = State::Str,
                    0x20..=0x2f => self.state = State::EscInt,
                    0x1b => {
                        self.restart_at(input, i, &mut pass_from, &mut seq_start, out);
                    }
                    0x18 | 0x1a => complete = Some(None),
                    0x30..=0x7e => complete = Some(if b == b'c' { Some(Class::Reset) } else { None }),
                    0x80.. => complete = Some(None),
                    _ => {}
                },
                State::EscInt => match b {
                    0x20..=0x2f => {}
                    0x1b => self.restart_at(input, i, &mut pass_from, &mut seq_start, out),
                    _ => complete = Some(None),
                },
                State::Csi => match b {
                    0x20..=0x3f => {
                        if self.csi.len() < CSI_LIMIT {
                            self.csi.push(b);
                        }
                    }
                    0x40..=0x7e => {
                        self.csi.push(b);
                        complete = Some(classify_csi(&self.csi));
                    }
                    0x1b => self.restart_at(input, i, &mut pass_from, &mut seq_start, out),
                    0x18 | 0x1a => complete = Some(None),
                    _ => {}
                },
                State::Str => match b {
                    0x07 | 0x18 | 0x1a => complete = Some(None),
                    0x1b => self.state = State::StrEsc,
                    _ => {}
                },
                State::StrEsc => {
                    if b == b'\\' {
                        complete = Some(None);
                    } else {
                        // The ESC ended the string and begins a new sequence; look at `b` again
                        // in the escape state.
                        if i == 0 {
                            if !self.overflow {
                                // The ESC is the last held byte.
                                self.held.pop();
                                if !self.held.is_empty() {
                                    out(Item::Data(&std::mem::take(&mut self.held)));
                                }
                                self.held.push(0x1b);
                            }
                            // With overflow the ESC was already passed on; keep passing through.
                        } else {
                            let esc = i - 1;
                            if !self.held.is_empty() {
                                out(Item::Data(&std::mem::take(&mut self.held)));
                                out(Item::Data(&input[pass_from..esc]));
                                pass_from = esc;
                            }
                            self.overflow = false;
                            seq_start = Some(esc);
                        }
                        self.state = State::Esc;
                        continue;
                    }
                }
            }
            if let Some(class) = complete {
                self.finish_seq(input, i + 1, &mut pass_from, &mut seq_start, class, out);
            } else if self.state != State::Ground && !self.overflow {
                let held_len = self.held.len() + seq_start.map_or(i + 1, |s| i + 1 - s);
                if held_len > HOLD_LIMIT {
                    // Give up holding this sequence: release it as data.
                    if !self.held.is_empty() {
                        out(Item::Data(&std::mem::take(&mut self.held)));
                    }
                    seq_start = None;
                    self.overflow = true;
                }
            }
            i += 1;
        }

        // End of input.
        if self.state == State::Ground || self.overflow {
            if pass_from < input.len() {
                out(Item::Data(&input[pass_from..]));
            }
        } else {
            match seq_start {
                Some(s) => {
                    if pass_from < s {
                        out(Item::Data(&input[pass_from..s]));
                    }
                    self.held.extend_from_slice(&input[s..]);
                }
                None => {
                    if !self.held.is_empty() {
                        self.held.extend_from_slice(&input[pass_from..]);
                    } else if pass_from < input.len() {
                        out(Item::Data(&input[pass_from..]));
                    }
                }
            }
        }
    }

    /// A new ESC arrived inside an unfinished sequence: what came before is finished as data and
    /// a new sequence starts at `i`.
    fn restart_at(
        &mut self,
        input: &[u8],
        i: usize,
        pass_from: &mut usize,
        seq_start: &mut Option<usize>,
        out: &mut dyn FnMut(Item<'_>),
    ) {
        if !self.held.is_empty() {
            out(Item::Data(&std::mem::take(&mut self.held)));
        }
        if *pass_from < i {
            out(Item::Data(&input[*pass_from..i]));
        }
        *pass_from = i;
        *seq_start = Some(i);
        self.overflow = false;
        self.state = State::Esc;
    }

    /// The current sequence ends before `end`. Reports it, separately if it is of interest.
    fn finish_seq(
        &mut self,
        input: &[u8],
        end: usize,
        pass_from: &mut usize,
        seq_start: &mut Option<usize>,
        class: Option<Class>,
        out: &mut dyn FnMut(Item<'_>),
    ) {
        let overflow = std::mem::take(&mut self.overflow);
        self.state = State::Ground;
        let start = seq_start.take();
        match class {
            Some(class) if !overflow => {
                if !self.held.is_empty() {
                    // The sequence began in an earlier call.
                    let mut seq = std::mem::take(&mut self.held);
                    seq.extend_from_slice(&input[*pass_from..end]);
                    out(Item::Seq(&seq, class));
                } else {
                    let s = start.unwrap_or(*pass_from);
                    if *pass_from < s {
                        out(Item::Data(&input[*pass_from..s]));
                    }
                    out(Item::Seq(&input[s..end], class));
                }
                *pass_from = end;
            }
            _ => {
                if !self.held.is_empty() {
                    let mut held = std::mem::take(&mut self.held);
                    held.extend_from_slice(&input[*pass_from..end]);
                    out(Item::Data(&held));
                    *pass_from = end;
                }
                // Otherwise the sequence stays part of the pending data run.
            }
        }
    }
}

fn classify_csi(csi: &[u8]) -> Option<Class> {
    let (&last, body) = csi.split_last()?;
    let (private, params) = match body.first() {
        Some(b'?') => (true, &body[1..]),
        Some(b'<' | b'=' | b'>') => return None,
        _ => (false, body),
    };
    if params.iter().any(|b| (0x20..=0x2f).contains(b)) {
        // Intermediates: not one of ours.
        return None;
    }
    let nums: Vec<Option<u16>> = params
        .split(|b| *b == b';')
        .map(|p| {
            let p = p.split(|b| *b == b':').next().unwrap_or(&[]);
            std::str::from_utf8(p).ok().and_then(|s| s.parse().ok())
        })
        .collect();
    match (private, last) {
        (true, b'h') => {
            if nums == [Some(2026)] {
                Some(Class::SyncBegin)
            } else if nums.iter().any(|n| matches!(n, Some(47 | 1047 | 1049))) {
                Some(Class::AltEnter)
            } else {
                None
            }
        }
        (true, b'l') if nums == [Some(2026)] => Some(Class::SyncEnd),
        (false, b'J') if nums == [Some(3)] => Some(Class::ClearHistory),
        (false, b'r') => Some(Class::ScrollRegion(
            nums.first().copied().flatten().filter(|n| *n > 0),
            nums.get(1).copied().flatten().filter(|n| *n > 0),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&[u8]]) -> Vec<(String, Option<Class>)> {
        let mut sc = Scanner::default();
        let mut v = Vec::new();
        for c in chunks {
            sc.scan(c, &mut |item| match item {
                Item::Data(d) => v.push((String::from_utf8_lossy(d).into_owned(), None)),
                Item::Seq(s, c) => v.push((String::from_utf8_lossy(s).into_owned(), Some(c))),
            });
        }
        v
    }

    fn data(v: &[(String, Option<Class>)]) -> String {
        v.iter().map(|(s, _)| s.as_str()).collect()
    }

    #[test]
    fn splits_at_sync_markers() {
        let v = run(&[b"a\x1b[?2026hb\x1b[1mc\x1b[?2026ld"]);
        assert_eq!(
            v,
            vec![
                ("a".into(), None),
                ("\x1b[?2026h".into(), Some(Class::SyncBegin)),
                ("b\x1b[1mc".into(), None),
                ("\x1b[?2026l".into(), Some(Class::SyncEnd)),
                ("d".into(), None),
            ]
        );
    }

    #[test]
    fn holds_back_split_sequences() {
        let v = run(&[b"ab\x1b[?20", b"26hc\xe7", b"\x95\x8c"]);
        assert_eq!(v[0], ("ab".into(), None));
        assert_eq!(v[1], ("\x1b[?2026h".into(), Some(Class::SyncBegin)));
        assert_eq!(data(&v[2..]), "c界");
        // The incomplete UTF-8 character was held until complete.
        assert_eq!(v[2], ("c".into(), None));
    }

    #[test]
    fn every_byte_is_reported_once() {
        let input: &[u8] =
            b"x\x1b]0;title\x07y\x1b]8;;http://a\x1b\\link\x1b]8;;\x1b\\\x1b[3Jz\x1bc\x1b[2;5r\x1b[?1049h";
        for split in 0..input.len() {
            let mut sc = Scanner::default();
            let mut out = Vec::new();
            let mut classes = Vec::new();
            for c in [&input[..split], &input[split..]] {
                sc.scan(c, &mut |item| match item {
                    Item::Data(d) => out.extend_from_slice(d),
                    Item::Seq(s, c) => {
                        out.extend_from_slice(s);
                        classes.push(c);
                    }
                });
            }
            assert_eq!(out, input, "split at {split}");
            assert_eq!(
                classes,
                vec![
                    Class::ClearHistory,
                    Class::Reset,
                    Class::ScrollRegion(Some(2), Some(5)),
                    Class::AltEnter
                ],
                "split at {split}"
            );
            assert_eq!(sc.held(), 0);
        }
    }

    #[test]
    fn esc_inside_string_starts_new_sequence() {
        let v = run(&[b"\x1b]0;t\x1b[?2026h"]);
        assert_eq!(data(&v), "\x1b]0;t\x1b[?2026h");
        assert_eq!(v.last().unwrap().1, Some(Class::SyncBegin));
    }

    #[test]
    fn long_strings_pass_through() {
        let mut big = b"\x1b]52;c;".to_vec();
        big.extend(std::iter::repeat_n(b'A', 10_000));
        let mut sc = Scanner::default();
        let mut n = 0;
        sc.scan(&big, &mut |item| {
            if let Item::Data(d) = item {
                n += d.len()
            }
        });
        assert_eq!(n, big.len());
        sc.scan(b"\x07", &mut |_| {});
        assert_eq!(sc.held(), 0);
    }
}
