//! Semantic input, encoded to bytes according to the terminal's current modes.
//!
//! Keys follow xterm's encoding, with application cursor keys, and the kitty keyboard
//! protocol's flags when the application pushed them (only press events are sent). Mouse events
//! use the X10, UTF-8 or SGR encoding the application chose, and are dropped when it did not ask
//! for mouse reporting, except that the wheel sends arrow keys on the alternate screen when
//! alternate scroll is on, as most terminals do.

use bitflags::bitflags;

use crate::screen::{KittyFlags, Modes, MouseEncoding, MouseMode};

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    pub struct Mods: u8 {
        const SHIFT = 1;
        const ALT   = 2;
        const CTRL  = 4;
        const SUPER = 8;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A key that produces a character. With Shift, give the shifted character (`'A'`).
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1 to F12.
    F(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseKind {
    Press(MouseButton),
    Release(MouseButton),
    /// Motion, with the button held if any.
    Move(Option<MouseButton>),
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

/// A mouse event at a cell, zero-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Mouse {
    pub kind: MouseKind,
    pub row: u16,
    pub col: u16,
    pub mods: Mods,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Input {
    Key(Key, Mods),
    /// Typed text, as if each character was a key press.
    Text(String),
    /// Pasted text, bracketed when the application asked for it.
    Paste(String),
    Mouse(Mouse),
    /// Focus gained (`true`) or lost.
    Focus(bool),
}

impl Input {
    pub fn key(key: Key) -> Input {
        Input::Key(key, Mods::empty())
    }

    /// A key from a name such as `enter`, `ctrl+c`, `shift+tab`, `alt+f`, `f5` or `a`.
    pub fn parse_key(spec: &str) -> Option<Input> {
        parse_key(spec).map(|(k, m)| Input::Key(k, m))
    }
}

/// Parses a key name such as `enter`, `ctrl+c`, `shift+tab`, `alt+left` or `f5`.
pub fn parse_key(spec: &str) -> Option<(Key, Mods)> {
    let mut mods = Mods::empty();
    let parts: Vec<&str> = if spec == "+" {
        vec!["+"]
    } else {
        spec.split('+').collect()
    };
    let (last, rest) = parts.split_last()?;
    for m in rest {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Mods::CTRL,
            "alt" | "meta" | "opt" | "option" => mods |= Mods::ALT,
            "shift" => mods |= Mods::SHIFT,
            "super" | "cmd" | "win" => mods |= Mods::SUPER,
            _ => return None,
        }
    }
    let lower = last.to_ascii_lowercase();
    let key = match lower.as_str() {
        "enter" | "return" | "cr" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" | "bs" => Key::Backspace,
        "esc" | "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "insert" | "ins" => Key::Insert,
        "delete" | "del" => Key::Delete,
        "space" => Key::Char(' '),
        f if f.len() >= 2
            && f.starts_with('f')
            && f[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) =>
        {
            Key::F(f[1..].parse().ok()?)
        }
        _ => {
            let mut chars = last.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Key::Char(c)
        }
    };
    Some((key, mods))
}

/// Encodes `input` for a terminal in `modes`. Returns nothing for input the application did not
/// ask for (mouse without mouse reporting, focus without focus events).
pub fn encode(input: &Input, modes: &Modes) -> Vec<u8> {
    let mut out = Vec::new();
    match input {
        Input::Key(key, mods) => encode_key(&mut out, *key, *mods, modes),
        Input::Text(text) => {
            if modes.kitty.contains(KittyFlags::REPORT_ALL_KEYS_AS_ESC) {
                for c in text.chars() {
                    let key = match c {
                        '\r' | '\n' => Key::Enter,
                        '\t' => Key::Tab,
                        '\x7f' => Key::Backspace,
                        '\x1b' => Key::Escape,
                        c => Key::Char(c),
                    };
                    let mods = if c.is_uppercase() {
                        Mods::SHIFT
                    } else {
                        Mods::empty()
                    };
                    encode_key(&mut out, key, mods, modes);
                }
            } else {
                out.extend_from_slice(text.as_bytes());
            }
        }
        Input::Paste(text) => {
            if modes.bracketed_paste {
                out.extend_from_slice(b"\x1b[200~");
                // A paste cannot end the bracket early.
                out.extend(text.bytes().filter(|b| *b != 0x1b));
                out.extend_from_slice(b"\x1b[201~");
            } else {
                let text = text.replace("\r\n", "\r").replace('\n', "\r");
                out.extend_from_slice(text.as_bytes());
            }
        }
        Input::Mouse(m) => encode_mouse(&mut out, m, modes),
        Input::Focus(focused) => {
            if modes.focus_events {
                out.extend_from_slice(if *focused { b"\x1b[I" } else { b"\x1b[O" });
            }
        }
    }
    out
}

fn mod_param(mods: Mods) -> u8 {
    1 + mods.bits()
}

fn encode_key(out: &mut Vec<u8>, key: Key, mods: Mods, modes: &Modes) {
    let kitty = modes.kitty;
    if kitty.intersects(KittyFlags::DISAMBIGUATE | KittyFlags::REPORT_ALL_KEYS_AS_ESC)
        && encode_kitty(out, key, mods, kitty)
    {
        return;
    }
    let m = mod_param(mods);
    let csi_letter = |out: &mut Vec<u8>, letter: u8, app: bool| {
        if mods.is_empty() {
            if app {
                out.extend_from_slice(&[0x1b, b'O', letter]);
            } else {
                out.extend_from_slice(&[0x1b, b'[', letter]);
            }
        } else {
            out.extend_from_slice(format!("\x1b[1;{m}").as_bytes());
            out.push(letter);
        }
    };
    let tilde = |out: &mut Vec<u8>, n: u8| {
        if mods.is_empty() {
            out.extend_from_slice(format!("\x1b[{n}~").as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b[{n};{m}~").as_bytes());
        }
    };
    match key {
        Key::Char(c) => {
            let mut c = c;
            if mods.contains(Mods::SHIFT) && c.is_ascii_lowercase() {
                c = c.to_ascii_uppercase();
            }
            if mods.contains(Mods::ALT) {
                out.push(0x1b);
            }
            if mods.contains(Mods::CTRL)
                && let Some(b) = ctrl_byte(c)
            {
                out.push(b);
                return;
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        Key::Enter => {
            if mods.contains(Mods::ALT) {
                out.push(0x1b);
            }
            out.extend_from_slice(if modes.newline { b"\r\n" } else { b"\r" });
        }
        Key::Tab => {
            if mods.contains(Mods::SHIFT) {
                out.extend_from_slice(b"\x1b[Z");
            } else {
                if mods.contains(Mods::ALT) {
                    out.push(0x1b);
                }
                out.push(b'\t');
            }
        }
        Key::Backspace => {
            if mods.contains(Mods::ALT) {
                out.push(0x1b);
            }
            out.push(if mods.contains(Mods::CTRL) { 0x08 } else { 0x7f });
        }
        Key::Escape => {
            if mods.contains(Mods::ALT) {
                out.push(0x1b);
            }
            out.push(0x1b);
        }
        Key::Up => csi_letter(out, b'A', modes.app_cursor),
        Key::Down => csi_letter(out, b'B', modes.app_cursor),
        Key::Right => csi_letter(out, b'C', modes.app_cursor),
        Key::Left => csi_letter(out, b'D', modes.app_cursor),
        Key::Home => csi_letter(out, b'H', modes.app_cursor),
        Key::End => csi_letter(out, b'F', modes.app_cursor),
        Key::Insert => tilde(out, 2),
        Key::Delete => tilde(out, 3),
        Key::PageUp => tilde(out, 5),
        Key::PageDown => tilde(out, 6),
        Key::F(n @ 1..=4) => csi_letter(out, b"PQRS"[n as usize - 1], true),
        Key::F(n) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            tilde(out, code)
        }
    }
}

/// The control character for Ctrl plus `c`, in xterm's table.
fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// Kitty keyboard protocol encoding. Returns false when the legacy encoding applies.
fn encode_kitty(out: &mut Vec<u8>, key: Key, mods: Mods, flags: KittyFlags) -> bool {
    let all = flags.contains(KittyFlags::REPORT_ALL_KEYS_AS_ESC);
    let m = mod_param(mods);
    let csi_u = |out: &mut Vec<u8>, code: u32, alt: Option<u32>, text: Option<char>| {
        let mut s = format!("\x1b[{code}");
        if let Some(a) = alt.filter(|_| flags.contains(KittyFlags::REPORT_ALTERNATE_KEYS)) {
            s.push_str(&format!(":{a}"));
        }
        let text = text.filter(|_| all && flags.contains(KittyFlags::REPORT_ASSOCIATED_TEXT));
        if m != 1 || text.is_some() {
            s.push_str(&format!(";{m}"));
        }
        if let Some(t) = text {
            s.push_str(&format!(";{}", t as u32));
        }
        s.push('u');
        out.extend_from_slice(s.as_bytes());
    };
    match key {
        Key::Char(c) => {
            let text_mods = mods.difference(Mods::SHIFT).is_empty();
            if text_mods && !all {
                return false;
            }
            let base = if c.is_uppercase() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                c
            };
            let shifted = (mods.contains(Mods::SHIFT) && base != c).then_some(c as u32);
            let text = (text_mods).then_some(c);
            csi_u(out, base as u32, shifted, text);
            true
        }
        Key::Escape => {
            csi_u(out, 27, None, None);
            true
        }
        Key::Enter | Key::Tab | Key::Backspace => {
            if mods.is_empty() && !all {
                return false;
            }
            let code = match key {
                Key::Enter => 13,
                Key::Tab => 9,
                _ => 127,
            };
            csi_u(out, code, None, None);
            true
        }
        // Functional keys keep the legacy CSI forms, which kitty shares.
        _ => false,
    }
}

fn encode_mouse(out: &mut Vec<u8>, m: &Mouse, modes: &Modes) {
    let wheel = matches!(
        m.kind,
        MouseKind::ScrollUp | MouseKind::ScrollDown | MouseKind::ScrollLeft | MouseKind::ScrollRight
    );
    if modes.mouse == MouseMode::None {
        if wheel && modes.alt_screen && modes.alternate_scroll {
            let key = match m.kind {
                MouseKind::ScrollUp => Key::Up,
                MouseKind::ScrollDown => Key::Down,
                _ => return,
            };
            let plain = Modes {
                kitty: KittyFlags::empty(),
                ..*modes
            };
            encode_key(out, key, Mods::empty(), &plain);
        }
        return;
    }
    let button_code = |b: MouseButton| match b {
        MouseButton::Left => 0u32,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (mut code, release) = match m.kind {
        MouseKind::Press(b) => (button_code(b), false),
        MouseKind::Release(b) => (
            if modes.mouse_encoding == MouseEncoding::Sgr {
                button_code(b)
            } else {
                3
            },
            true,
        ),
        MouseKind::Move(b) => {
            match (modes.mouse, b) {
                (MouseMode::Motion, _) | (MouseMode::Drag, Some(_)) => {}
                _ => return,
            }
            (32 + b.map_or(3, button_code), false)
        }
        MouseKind::ScrollUp => (64, false),
        MouseKind::ScrollDown => (65, false),
        MouseKind::ScrollLeft => (66, false),
        MouseKind::ScrollRight => (67, false),
    };
    if m.mods.contains(Mods::SHIFT) {
        code += 4;
    }
    if m.mods.contains(Mods::ALT) {
        code += 8;
    }
    if m.mods.contains(Mods::CTRL) {
        code += 16;
    }
    let (x, y) = (m.col as u32 + 1, m.row as u32 + 1);
    match modes.mouse_encoding {
        MouseEncoding::Sgr => {
            out.extend_from_slice(
                format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).as_bytes(),
            );
        }
        MouseEncoding::Default => {
            if x > 223 || y > 223 {
                return;
            }
            out.extend_from_slice(b"\x1b[M");
            out.extend_from_slice(&[(32 + code) as u8, (32 + x) as u8, (32 + y) as u8]);
        }
        MouseEncoding::Utf8 => {
            out.extend_from_slice(b"\x1b[M");
            for v in [32 + code, 32 + x, 32 + y] {
                if let Some(c) = char::from_u32(v) {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(input: Input, modes: Modes) -> String {
        String::from_utf8(encode(&input, &modes)).unwrap()
    }

    fn key(spec: &str) -> Input {
        Input::parse_key(spec).unwrap()
    }

    #[test]
    fn legacy_keys() {
        let m = Modes::default();
        assert_eq!(enc(key("a"), m), "a");
        assert_eq!(enc(key("shift+a"), m), "A");
        assert_eq!(enc(key("ctrl+c"), m), "\x03");
        assert_eq!(enc(key("alt+f"), m), "\x1bf");
        assert_eq!(enc(key("ctrl+alt+x"), m), "\x1b\x18");
        assert_eq!(enc(key("enter"), m), "\r");
        assert_eq!(enc(key("tab"), m), "\t");
        assert_eq!(enc(key("shift+tab"), m), "\x1b[Z");
        assert_eq!(enc(key("backspace"), m), "\x7f");
        assert_eq!(enc(key("esc"), m), "\x1b");
        assert_eq!(enc(key("up"), m), "\x1b[A");
        assert_eq!(enc(key("ctrl+left"), m), "\x1b[1;5D");
        assert_eq!(enc(key("shift+alt+end"), m), "\x1b[1;4F");
        assert_eq!(enc(key("delete"), m), "\x1b[3~");
        assert_eq!(enc(key("ctrl+pagedown"), m), "\x1b[6;5~");
        assert_eq!(enc(key("f1"), m), "\x1bOP");
        assert_eq!(enc(key("shift+f3"), m), "\x1b[1;2R");
        assert_eq!(enc(key("f12"), m), "\x1b[24~");
        assert_eq!(enc(key("ctrl+space"), m), "\0");
    }

    #[test]
    fn app_cursor_keys() {
        let m = Modes {
            app_cursor: true,
            ..Default::default()
        };
        assert_eq!(enc(key("up"), m), "\x1bOA");
        assert_eq!(enc(key("home"), m), "\x1bOH");
        assert_eq!(enc(key("ctrl+up"), m), "\x1b[1;5A");
    }

    #[test]
    fn kitty_disambiguate() {
        let m = Modes {
            kitty: KittyFlags::DISAMBIGUATE,
            ..Default::default()
        };
        assert_eq!(enc(key("a"), m), "a");
        assert_eq!(enc(key("shift+a"), m), "A");
        assert_eq!(enc(key("ctrl+c"), m), "\x1b[99;5u");
        assert_eq!(enc(key("alt+a"), m), "\x1b[97;3u");
        assert_eq!(enc(key("esc"), m), "\x1b[27u");
        assert_eq!(enc(key("enter"), m), "\r");
        assert_eq!(enc(key("shift+enter"), m), "\x1b[13;2u");
        assert_eq!(enc(key("ctrl+backspace"), m), "\x1b[127;5u");
        assert_eq!(enc(key("up"), m), "\x1b[A");
        assert_eq!(enc(Input::Text("hi".into()), m), "hi");
    }

    #[test]
    fn kitty_all_keys() {
        let m = Modes {
            kitty: KittyFlags::DISAMBIGUATE
                | KittyFlags::REPORT_ALL_KEYS_AS_ESC
                | KittyFlags::REPORT_ALTERNATE_KEYS,
            ..Default::default()
        };
        assert_eq!(enc(key("a"), m), "\x1b[97u");
        assert_eq!(enc(Input::Key(Key::Char('A'), Mods::SHIFT), m), "\x1b[97:65;2u");
        assert_eq!(enc(key("enter"), m), "\x1b[13u");
        assert_eq!(enc(Input::Text("ab".into()), m), "\x1b[97u\x1b[98u");
        let m = Modes {
            kitty: KittyFlags::REPORT_ALL_KEYS_AS_ESC | KittyFlags::REPORT_ASSOCIATED_TEXT,
            ..Default::default()
        };
        assert_eq!(enc(key("x"), m), "\x1b[120;1;120u");
    }

    #[test]
    fn paste() {
        let m = Modes::default();
        assert_eq!(enc(Input::Paste("a\nb\r\nc".into()), m), "a\rb\rc");
        let m = Modes {
            bracketed_paste: true,
            ..Default::default()
        };
        assert_eq!(
            enc(Input::Paste("x\x1b[201~y".into()), m),
            "\x1b[200~x[201~y\x1b[201~"
        );
    }

    #[test]
    fn mouse() {
        let at = |kind| {
            Input::Mouse(Mouse {
                kind,
                row: 4,
                col: 9,
                mods: Mods::empty(),
            })
        };
        let none = Modes::default();
        assert_eq!(enc(at(MouseKind::Press(MouseButton::Left)), none), "");
        let alt = Modes {
            alt_screen: true,
            ..Default::default()
        };
        assert_eq!(enc(at(MouseKind::ScrollUp), alt), "\x1b[A");
        let click = Modes {
            mouse: MouseMode::Click,
            ..Default::default()
        };
        assert_eq!(enc(at(MouseKind::Press(MouseButton::Left)), click), "\x1b[M *%");
        assert_eq!(enc(at(MouseKind::Release(MouseButton::Left)), click), "\x1b[M#*%");
        assert_eq!(enc(at(MouseKind::Move(Some(MouseButton::Left))), click), "");
        let sgr = Modes {
            mouse: MouseMode::Drag,
            mouse_encoding: MouseEncoding::Sgr,
            ..Default::default()
        };
        assert_eq!(
            enc(at(MouseKind::Press(MouseButton::Right)), sgr),
            "\x1b[<2;10;5M"
        );
        assert_eq!(
            enc(at(MouseKind::Release(MouseButton::Right)), sgr),
            "\x1b[<2;10;5m"
        );
        assert_eq!(
            enc(at(MouseKind::Move(Some(MouseButton::Left))), sgr),
            "\x1b[<32;10;5M"
        );
        assert_eq!(enc(at(MouseKind::Move(None)), sgr), "");
        assert_eq!(enc(at(MouseKind::ScrollDown), sgr), "\x1b[<65;10;5M");
        let motion = Modes {
            mouse: MouseMode::Motion,
            mouse_encoding: MouseEncoding::Sgr,
            ..Default::default()
        };
        assert_eq!(enc(at(MouseKind::Move(None)), motion), "\x1b[<35;10;5M");
        let ctrl = Input::Mouse(Mouse {
            kind: MouseKind::Press(MouseButton::Left),
            row: 0,
            col: 0,
            mods: Mods::CTRL,
        });
        assert_eq!(enc(ctrl, sgr), "\x1b[<16;1;1M");
    }

    #[test]
    fn focus() {
        assert_eq!(enc(Input::Focus(true), Modes::default()), "");
        let m = Modes {
            focus_events: true,
            ..Default::default()
        };
        assert_eq!(enc(Input::Focus(true), m), "\x1b[I");
        assert_eq!(enc(Input::Focus(false), m), "\x1b[O");
    }

    #[test]
    fn modes_from_a_real_emulator() {
        let mut e = crate::Emulator::new(Default::default());
        e.feed(b"\x1b[?1h\x1b[>1u");
        let m = e.screen().modes();
        assert_eq!(enc(key("up"), m), "\x1bOA");
        assert_eq!(enc(key("ctrl+c"), m), "\x1b[99;5u");
        e.feed(b"\x1b[<u");
        assert_eq!(enc(key("ctrl+c"), e.screen().modes()), "\x03");
    }
}
