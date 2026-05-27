//! Keyboard input handling for terminal applications.
//!
//! Supports both legacy terminal sequences and the Kitty CSI-u protocol.
//!
//! Simplified from the TypeScript `keys.ts` (1,400 lines → ~300 lines).
//!
//! API:
//! - `parse_key(input)` -- parse raw terminal input into a `KeyEvent`
//! - `matches_key(event, key_id)` -- check if a `KeyEvent` matches a key
//!   description string like `"ctrl+c"`, `"enter"`, `"alt+left"`
//! - `last_event_type()` -- get the event type of the last parsed CSI-u key

use std::cell::Cell;
use std::fmt;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Key modifier flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeyModifiers {
    pub const fn none() -> Self {
        Self { ctrl: false, alt: false, shift: false }
    }
}

impl fmt::Display for KeyModifiers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("ctrl");
        }
        if self.alt {
            parts.push("alt");
        }
        if self.shift {
            parts.push("shift");
        }
        write!(f, "{}", parts.join("+"))
    }
}

/// The base key code (without modifiers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Backspace,
    Enter,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
    F(u8),
    // Keypad keys (Kitty protocol numeric codepoints 57399-57414)
    KpEnter,
    Kp0,
    Kp1,
    Kp2,
    Kp3,
    Kp4,
    Kp5,
    Kp6,
    Kp7,
    Kp8,
    Kp9,
    KpDivide,
    KpMultiply,
    KpSubtract,
    KpAdd,
    KpDecimal,
}

/// Event type for Kitty keyboard protocol (flag 2).
///
/// Indicates whether a key event is a press, repeat, or release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventType {
    Press,
    Repeat,
    Release,
}

thread_local! {
    static LAST_EVENT_TYPE: Cell<KeyEventType> = const { Cell::new(KeyEventType::Press) };
}

/// Get the event type of the last parsed CSI-u key event.
///
/// This is only meaningful when the Kitty keyboard protocol with flag 2 is active.
/// Returns `Press` by default (or when parsing non-CSI-u sequences).
pub fn last_event_type() -> KeyEventType {
    LAST_EVENT_TYPE.with(|cell| cell.get())
}

/// A parsed key event including modifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    pub fn new(code: KeyCode) -> Self {
        Self { code, modifiers: KeyModifiers::none() }
    }
}

impl fmt::Display for KeyEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key_name = match &self.code {
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Backspace => "backspace".to_string(),
            KeyCode::Enter => "enter".to_string(),
            KeyCode::Tab => "tab".to_string(),
            KeyCode::Escape => "escape".to_string(),
            KeyCode::Up => "up".to_string(),
            KeyCode::Down => "down".to_string(),
            KeyCode::Left => "left".to_string(),
            KeyCode::Right => "right".to_string(),
            KeyCode::Home => "home".to_string(),
            KeyCode::End => "end".to_string(),
            KeyCode::PageUp => "pageup".to_string(),
            KeyCode::PageDown => "pagedown".to_string(),
            KeyCode::Delete => "delete".to_string(),
            KeyCode::Insert => "insert".to_string(),
            KeyCode::F(n) => format!("f{n}"),
            KeyCode::KpEnter => "kp_enter".to_string(),
            KeyCode::Kp0 => "kp_0".to_string(),
            KeyCode::Kp1 => "kp_1".to_string(),
            KeyCode::Kp2 => "kp_2".to_string(),
            KeyCode::Kp3 => "kp_3".to_string(),
            KeyCode::Kp4 => "kp_4".to_string(),
            KeyCode::Kp5 => "kp_5".to_string(),
            KeyCode::Kp6 => "kp_6".to_string(),
            KeyCode::Kp7 => "kp_7".to_string(),
            KeyCode::Kp8 => "kp_8".to_string(),
            KeyCode::Kp9 => "kp_9".to_string(),
            KeyCode::KpDivide => "kp_divide".to_string(),
            KeyCode::KpMultiply => "kp_multiply".to_string(),
            KeyCode::KpSubtract => "kp_subtract".to_string(),
            KeyCode::KpAdd => "kp_add".to_string(),
            KeyCode::KpDecimal => "kp_decimal".to_string(),
        };

        let mods = self.modifiers.to_string();
        if mods.is_empty() {
            write!(f, "{key_name}")
        } else {
            write!(f, "{mods}+{key_name}")
        }
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Functional codepoint normalization
// ---------------------------------------------------------------------------

/// Normalize a Kitty private-range codepoint (57399-57426) into a `KeyCode`.
///
/// These codepoints represent keypad keys, arrows, and other functional keys
/// in the Kitty keyboard protocol. Returns `Some(KeyCode)` if the codepoint
/// is recognized, or `None` for regular character codepoints.
fn normalize_kitty_functional_codepoint(codepoint: u32) -> Option<KeyCode> {
    match codepoint {
        57399 => Some(KeyCode::Kp0),
        57400 => Some(KeyCode::Kp1),
        57401 => Some(KeyCode::Kp2),
        57402 => Some(KeyCode::Kp3),
        57403 => Some(KeyCode::Kp4),
        57404 => Some(KeyCode::Kp5),
        57405 => Some(KeyCode::Kp6),
        57406 => Some(KeyCode::Kp7),
        57407 => Some(KeyCode::Kp8),
        57408 => Some(KeyCode::Kp9),
        57409 => Some(KeyCode::KpDecimal),
        57410 => Some(KeyCode::KpDivide),
        57411 => Some(KeyCode::KpMultiply),
        57412 => Some(KeyCode::KpSubtract),
        57413 => Some(KeyCode::KpAdd),
        57414 => Some(KeyCode::KpEnter),
        // KP_EQUAL (57415) and KP_SEPARATOR (57416) map to standard ASCII
        57415 => Some(KeyCode::Char('=')),
        57416 => Some(KeyCode::Char(',')),
        // Arrows and functional keys mapped to existing variants
        57417 => Some(KeyCode::Left),
        57418 => Some(KeyCode::Right),
        57419 => Some(KeyCode::Up),
        57420 => Some(KeyCode::Down),
        57421 => Some(KeyCode::PageUp),
        57422 => Some(KeyCode::PageDown),
        57423 => Some(KeyCode::Home),
        57424 => Some(KeyCode::End),
        57425 => Some(KeyCode::Insert),
        57426 => Some(KeyCode::Delete),
        _ => None,
    }
}

/// Check if a codepoint corresponds to a known key on a standard US keyboard.
///
/// This determines whether the base layout key fallback should be used.
/// For non-Latin layouts (e.g., Cyrillic), pressing the 'C' physical key
/// produces a different codepoint. If that codepoint is NOT a known Latin
/// letter, digit, or symbol, we fall back to the base layout key code.
fn is_known_key(codepoint: u32) -> bool {
    // a-z
    if (97..=122).contains(&codepoint) {
        return true;
    }
    // 0-9
    if (48..=57).contains(&codepoint) {
        return true;
    }
    // Known symbols (matching TS SYMBOL_KEYS)
    matches!(
        char::from_u32(codepoint),
        Some(
            // Unshifted symbols
            '`' | '-' | '=' | '[' | ']' | '\\' | ';' | '\'' | ',' | '.' | '/'
            // Shifted symbols
            | '!' | '@' | '#' | '$' | '%' | '^' | '&' | '*' | '(' | ')' | '_'
            | '+' | '|' | '~' | '{' | '}' | ':' | '<' | '>' | '?'
        )
    )
}

// ---------------------------------------------------------------------------
// Single-byte parsing
// ---------------------------------------------------------------------------

fn parse_single_byte(b: u8) -> KeyEvent {
    match b {
        0x00 => KeyEvent {
            code: KeyCode::Char(' '),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        },
        0x01..=0x06 | 0x0a..=0x0c | 0x0e..=0x1a => {
            // Ctrl+A through Ctrl+Z (except 0x09=Tab, 0x0d=CR, 0x1b=ESC)
            let letter = (b + 96) as char;
            KeyEvent {
                code: KeyCode::Char(letter),
                modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
            }
        }
        0x07 => {
            // Ctrl+G (BEL) — also used as OSC terminator, but treat as Ctrl+G here
            KeyEvent {
                code: KeyCode::Char('g'),
                modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
            }
        }
        0x08 => KeyEvent::new(KeyCode::Backspace), // BS — treat as backspace
        0x09 => KeyEvent::new(KeyCode::Tab),
        0x0d => KeyEvent::new(KeyCode::Enter), // CR — treat as enter
        0x1b => KeyEvent::new(KeyCode::Escape),
        0x1c => KeyEvent {
            code: KeyCode::Char('\\'),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        },
        0x1d => KeyEvent {
            code: KeyCode::Char(']'),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        },
        0x1e => KeyEvent {
            code: KeyCode::Char('^'),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        },
        0x1f => KeyEvent {
            code: KeyCode::Char('_'),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        },
        0x20..=0x7e => KeyEvent::new(KeyCode::Char(b as char)),
        0x7f => KeyEvent::new(KeyCode::Backspace), // DEL
        _ => KeyEvent::new(KeyCode::Char(b as char)),
    }
}

// ---------------------------------------------------------------------------
// Modifier parsing helpers
// ---------------------------------------------------------------------------

/// Parse a colon-delimited modifier string into a 1-indexed modifier value
/// and an optional event type.
///
/// Input examples: `"5"` → (5, Press), `"5:2"` → (5, Repeat), `"5:3"` → (5, Release)
fn parse_modifier_and_event(mod_str: &str) -> Option<(u32, KeyEventType)> {
    let parts: Vec<&str> = mod_str.split(':').collect();
    let raw_mod: u32 = parts[0].parse().ok()?;
    let event_type = match parts.get(1) {
        Some(&"2") => KeyEventType::Repeat,
        Some(&"3") => KeyEventType::Release,
        _ => KeyEventType::Press,
    };
    Some((raw_mod, event_type))
}

/// Decode a 1-indexed CSI modifier value into our bitmask.
fn decode_csi_mod(raw_mod: u32) -> KeyModifiers {
    let m = raw_mod.saturating_sub(1);
    KeyModifiers {
        shift: (m & 1) != 0,
        alt: (m & 2) != 0,
        ctrl: (m & 4) != 0,
    }
}

// ---------------------------------------------------------------------------
// CSI-u (Kitty keyboard protocol) parsing
// ---------------------------------------------------------------------------

/// Parse a CSI-u payload (the part between `ESC[` and `u`, without the `u`).
///
/// Full format (with Kitty flags 1 + 2 + 4):
///   `<codepoint>[:<shifted>[:<base>]][;<modifier>[:<event_type>]]`
///
/// With flag 2, event type is appended after modifier colon: 1=press, 2=repeat, 3=release.
/// With flag 4, alternate keys are appended after codepoint with colons.
///
/// Returns `None` if the payload is not a valid CSI-u sequence.
fn parse_csi_u_payload(payload: &str) -> Option<KeyEvent> {
    // Split on ';' to isolate modifier part
    let (code_part, mod_part) = if let Some(semi) = payload.rfind(';') {
        let (before, after) = payload.split_at(semi);
        (before, Some(&after[1..]))
    } else {
        (payload, None)
    };

    // Parse colon-separated values from the code part:
    //   <codepoint>[:<shifted>[:<base>]]
    let code_parts: Vec<&str> = code_part.split(':').collect();
    let codepoint: u32 = code_parts[0].parse().ok()?;

    // Shifted key (may be empty string meaning explicitly omitted, e.g. `code::base`)
    let _shifted_key: Option<u32> = code_parts.get(1).and_then(|s| {
        if s.is_empty() { None } else { s.parse().ok() }
    });

    // Base layout key — the key position on a standard PC-101 layout
    let base_layout_key: Option<u32> = code_parts.get(2).and_then(|s| s.parse().ok());

    // Parse modifier and event type from modifier part
    let (raw_mod, event_type) = match mod_part {
        Some(s) => parse_modifier_and_event(s).unwrap_or((1, KeyEventType::Press)),
        None => (1, KeyEventType::Press),
    };

    // Store event type for external querying
    LAST_EVENT_TYPE.with(|cell| cell.set(event_type));

    // Decode modifiers (1-indexed in CSI-u; subtract 1 to get bitmask).
    // bit 0 = shift, bit 1 = alt, bit 2 = ctrl
    let m = raw_mod.saturating_sub(1);
    let shift = (m & 1) != 0;
    let alt = (m & 2) != 0;
    let ctrl = (m & 4) != 0;
    let mods = KeyModifiers { ctrl, alt, shift };

    // 1. Functional codepoint normalization — map Kitty private range to KeyCode
    if let Some(kc) = normalize_kitty_functional_codepoint(codepoint) {
        return Some(KeyEvent { code: kc, modifiers: mods });
    }

    // 2. Control characters (1-26) with ctrl → Ctrl+letter
    if (1..=26).contains(&codepoint) && ctrl {
        let letter = char::from_u32(codepoint + 96).unwrap_or('?');
        return Some(KeyEvent {
            code: KeyCode::Char(letter),
            modifiers: mods,
        });
    }

    // 3. Special key codepoints → KeyCode variants
    if let Some(kc) = codepoint_to_special_key(codepoint) {
        return Some(KeyEvent { code: kc, modifiers: mods });
    }

    // 4. Determine effective codepoint with base layout key fallback.
    //    For non-Latin layouts (e.g., Cyrillic, Japanese), when the codepoint
    //    is not a recognised Latin letter, digit, or known symbol, use the
    //    base layout key code instead. This allows Ctrl+<Cyrillic letter> to
    //    match Ctrl+c when both keys are at the same physical position.
    let effective_cp = if let Some(base) = base_layout_key {
        if !is_known_key(codepoint) {
            base
        } else {
            codepoint
        }
    } else {
        codepoint
    };

    // 5. Normalize shifted uppercase letters to lowercase when shift is held.
    //    Terminal sends `\x1b[65;2u` (Shift+A, codepoint 65='A') but we want
    //    Char('a') + shift so that matches_key("shift+a") works correctly.
    let final_cp = if shift && (65..=90).contains(&effective_cp) {
        effective_cp + 32
    } else {
        effective_cp
    };

    let ch = char::from_u32(final_cp).unwrap_or('?');
    Some(KeyEvent {
        code: KeyCode::Char(ch),
        modifiers: mods,
    })
}

/// Map common control character codepoints to their `KeyCode` variants.
fn codepoint_to_special_key(codepoint: u32) -> Option<KeyCode> {
    match codepoint {
        27 => Some(KeyCode::Escape),
        9 => Some(KeyCode::Tab),
        13 => Some(KeyCode::Enter),
        127 => Some(KeyCode::Backspace),
        32 => Some(KeyCode::Char(' ')),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Legacy modifyOtherKeys parsing
// ---------------------------------------------------------------------------

/// Parse an xterm modifyOtherKeys sequence: `\x1b[27;<mod>;<code>~`
///
/// Format: `<mod>` is 1-indexed (2=shift, 3=alt, 5=ctrl, etc.),
/// `<code>` is the key codepoint.
fn parse_modify_other_keys(payload: &str) -> Option<KeyEvent> {
    // Format: 27;<mod>;<code>~
    let inner = payload.strip_suffix('~')?;
    let parts: Vec<&str> = inner.split(';').collect();
    if parts.len() != 3 || parts[0] != "27" {
        return None;
    }
    let raw_mod: u32 = parts[1].parse().ok()?;
    let code: u32 = parts[2].parse().ok()?;
    let mods = decode_csi_mod(raw_mod);

    // Control characters (1-26) with ctrl → Ctrl+letter
    if (1..=26).contains(&code) && mods.ctrl {
        let letter = char::from_u32(code + 96).unwrap_or('?');
        return Some(KeyEvent {
            code: KeyCode::Char(letter),
            modifiers: mods,
        });
    }

    // Special key codepoints
    if let Some(kc) = codepoint_to_special_key(code) {
        return Some(KeyEvent { code: kc, modifiers: mods });
    }

    let ch = char::from_u32(code)?;
    Some(KeyEvent {
        code: KeyCode::Char(ch),
        modifiers: mods,
    })
}

// ---------------------------------------------------------------------------
// CSI modified-key parsing (arrows, home/end, function keys)
// ---------------------------------------------------------------------------

/// Parse a CSI sequence payload (after `ESC[`) for modified keys.
/// Handles `1;<mod>[:<event>]X` (arrows), `1;<mod>[:<event>]H/F` (home/end),
/// `<num>;<mod>[:<event>]~` (func).
fn parse_csi_modified(payload: &str) -> Option<KeyEvent> {
    let bytes = payload.as_bytes();
    let last = *bytes.last()?;

    // --- Arrow keys / Home / End: `1;<mod>[:<event>]X` ---
    if bytes.starts_with(b"1;") && matches!(last, b'A' | b'B' | b'C' | b'D' | b'H' | b'F') {
        // Strip the final terminator byte before parsing modifier
        let inner = &payload[2..payload.len() - 1];
        let (raw_mod, event_type) = parse_modifier_and_event(inner)?;
        LAST_EVENT_TYPE.with(|cell| cell.set(event_type));
        let mods = decode_csi_mod(raw_mod);

        let code = match last {
            b'A' => KeyCode::Up,
            b'B' => KeyCode::Down,
            b'C' => KeyCode::Right,
            b'D' => KeyCode::Left,
            b'H' => KeyCode::Home,
            b'F' => KeyCode::End,
            _ => return None,
        };
        return Some(KeyEvent { code, modifiers: mods });
    }

    // --- Function / tilde codes: `<num>;<mod>[:<event>]~` ---
    if last == b'~' {
        let inner = &payload[..payload.len() - 1];
        let (num_str, mod_str) = if let Some(semi) = inner.rfind(';') {
            (&inner[..semi], Some(&inner[semi + 1..]))
        } else {
            (inner, None)
        };
        let num: u32 = num_str.parse().ok()?;
        let (raw_mod, event_type) = match mod_str {
            Some(s) => parse_modifier_and_event(s)?,
            None => (1, KeyEventType::Press),
        };
        LAST_EVENT_TYPE.with(|cell| cell.set(event_type));
        let mods = decode_csi_mod(raw_mod);

        let code = match num {
            1 | 7 => KeyCode::Home,
            2 => KeyCode::Insert,
            3 => KeyCode::Delete,
            4 | 8 => KeyCode::End,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            11..=24 => KeyCode::F((num - 10) as u8),
            25..=36 => KeyCode::F((num - 12) as u8), // 25=F13 .. 36=F24
            _ => return None,
        };
        return Some(KeyEvent { code, modifiers: mods });
    }

    None
}

// ---------------------------------------------------------------------------
// SS3 sequence parsing
// ---------------------------------------------------------------------------

fn parse_ss3(payload: &str) -> Option<KeyEvent> {
    let bytes = payload.as_bytes();
    if bytes.len() != 1 {
        return None;
    }
    match bytes[0] {
        b'A' => Some(KeyEvent::new(KeyCode::Up)),
        b'B' => Some(KeyEvent::new(KeyCode::Down)),
        b'C' => Some(KeyEvent::new(KeyCode::Right)),
        b'D' => Some(KeyEvent::new(KeyCode::Left)),
        b'H' => Some(KeyEvent::new(KeyCode::Home)),
        b'F' => Some(KeyEvent::new(KeyCode::End)),
        b'P' => Some(KeyEvent::new(KeyCode::F(1))),
        b'Q' => Some(KeyEvent::new(KeyCode::F(2))),
        b'R' => Some(KeyEvent::new(KeyCode::F(3))),
        b'S' => Some(KeyEvent::new(KeyCode::F(4))),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Meta (ESC + single byte) parsing
// ---------------------------------------------------------------------------

fn parse_meta(bytes: &[u8]) -> KeyEvent {
    debug_assert!(bytes.len() == 2 && bytes[0] == b'\x1b');
    let b = bytes[1];
    match b {
        b'\r' => KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers { ctrl: false, alt: true, shift: false },
        },
        0x7f | 0x08 => KeyEvent {
            code: KeyCode::Backspace,
            modifiers: KeyModifiers { ctrl: false, alt: true, shift: false },
        },
        0x1b => KeyEvent::new(KeyCode::Escape),
        0x01..=0x1a => {
            let letter = (b + 96) as char;
            KeyEvent {
                code: KeyCode::Char(letter),
                modifiers: KeyModifiers { ctrl: true, alt: true, shift: false },
            }
        }
        0x20..=0x7e => KeyEvent {
            code: KeyCode::Char(b as char),
            modifiers: KeyModifiers { ctrl: false, alt: true, shift: false },
        },
        _ => KeyEvent {
            code: KeyCode::Char(b as char),
            modifiers: KeyModifiers { ctrl: false, alt: true, shift: false },
        },
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse raw terminal input into a `KeyEvent`.
///
/// Handles:
/// - ASCII printable characters → `Char(c)`
/// - Ctrl+letter (bytes 0x01-0x1a) → `Char(c)` with ctrl modifier
/// - Escape sequences: `\x1b[A` → Up, etc.
/// - Legacy SS3 sequences: `\x1bOP` → F1, etc.
/// - Kitty CSI-u sequences: `\x1b[99;5u` → `Char('c')` with ctrl modifier
/// - Modified CSI keys: `\x1b[1;5A` → ctrl+Up
/// - Meta sequences: `\x1ba` → `Char('a')` with alt modifier
/// - Bracketed paste markers: `\x1b[200~`, `\x1b[201~`
/// - Legacy modifyOtherKeys: `\x1b[27;5;99~` → `Char('c')` with ctrl modifier
pub fn parse_key(input: &str) -> KeyEvent {
    if input.is_empty() {
        return KeyEvent::new(KeyCode::Escape);
    }

    let bytes = input.as_bytes();

    // --- Single byte ---
    if bytes.len() == 1 {
        return parse_single_byte(bytes[0]);
    }

    // --- Multi-byte escape sequences ---

    // CSI sequences: ESC [ ...
    if let Some(inner) = input.strip_prefix("\x1b[") {

        // Known fixed CSI sequences
        match inner {
            "A" => return KeyEvent::new(KeyCode::Up),
            "B" => return KeyEvent::new(KeyCode::Down),
            "C" => return KeyEvent::new(KeyCode::Right),
            "D" => return KeyEvent::new(KeyCode::Left),
            "H" => return KeyEvent::new(KeyCode::Home),
            "F" => return KeyEvent::new(KeyCode::End),
            "Z" => {
                return KeyEvent {
                    code: KeyCode::Tab,
                    modifiers: KeyModifiers { ctrl: false, alt: false, shift: true },
                }
            }
            "2~" => return KeyEvent::new(KeyCode::Insert),
            "3~" => return KeyEvent::new(KeyCode::Delete),
            "5~" => return KeyEvent::new(KeyCode::PageUp),
            "6~" => return KeyEvent::new(KeyCode::PageDown),
            "1~" | "7~" => return KeyEvent::new(KeyCode::Home),
            "4~" | "8~" => return KeyEvent::new(KeyCode::End),
            "200~" => {
                return KeyEvent {
                    code: KeyCode::Char('\u{e000}'),
                    modifiers: KeyModifiers::none(),
                }
            } // paste-start marker
            "201~" => {
                return KeyEvent {
                    code: KeyCode::Char('\u{e001}'),
                    modifiers: KeyModifiers::none(),
                }
            } // paste-end marker
            "11~" => return KeyEvent::new(KeyCode::F(1)),
            "12~" => return KeyEvent::new(KeyCode::F(2)),
            "13~" => return KeyEvent::new(KeyCode::F(3)),
            "14~" => return KeyEvent::new(KeyCode::F(4)),
            "15~" => return KeyEvent::new(KeyCode::F(5)),
            "17~" => return KeyEvent::new(KeyCode::F(6)),
            "18~" => return KeyEvent::new(KeyCode::F(7)),
            "19~" => return KeyEvent::new(KeyCode::F(8)),
            "20~" => return KeyEvent::new(KeyCode::F(9)),
            "21~" => return KeyEvent::new(KeyCode::F(10)),
            "23~" => return KeyEvent::new(KeyCode::F(11)),
            "24~" => return KeyEvent::new(KeyCode::F(12)),
            // F13-F24 tilde sequences
            "25~" => return KeyEvent::new(KeyCode::F(13)),
            "26~" => return KeyEvent::new(KeyCode::F(14)),
            "27~" => return KeyEvent::new(KeyCode::F(15)),
            "28~" => return KeyEvent::new(KeyCode::F(16)),
            "29~" => return KeyEvent::new(KeyCode::F(17)),
            "30~" => return KeyEvent::new(KeyCode::F(18)),
            "31~" => return KeyEvent::new(KeyCode::F(19)),
            "32~" => return KeyEvent::new(KeyCode::F(20)),
            "33~" => return KeyEvent::new(KeyCode::F(21)),
            "34~" => return KeyEvent::new(KeyCode::F(22)),
            "35~" => return KeyEvent::new(KeyCode::F(23)),
            "36~" => return KeyEvent::new(KeyCode::F(24)),
            _ => {}
        }

        // CSI-u: \x1b[<code>u or \x1b[<code>;<mod>[:<event>]u
        if let Some(payload) = inner.strip_suffix('u') {
            if let Some(ev) = parse_csi_u_payload(payload) {
                return ev;
            }
        }

        // Legacy modifyOtherKeys: \x1b[27;<mod>;<code>~
        if let Some(ev) = parse_modify_other_keys(inner) {
            return ev;
        }

        // CSI modified: \x1b[1;<mod>[:<event>]X or \x1b[<num>;<mod>[:<event>]~
        if let Some(ev) = parse_csi_modified(inner) {
            return ev;
        }

        // Fallback: treat as Unknown
        return KeyEvent {
            code: KeyCode::Char(input.chars().nth(2).unwrap_or('?')),
            modifiers: KeyModifiers::none(),
        };
    }

    // SS3 sequences: ESC O ...
    if let Some(inner) = input.strip_prefix("\x1bO") {
        if let Some(ev) = parse_ss3(inner) {
            return ev;
        }
        // Unknown SS3
        return KeyEvent::new(KeyCode::Char(input.chars().nth(2).unwrap_or('?')));
    }

    // Meta / Alt prefix: ESC + single byte
    if bytes.len() == 2 && bytes[0] == b'\x1b' {
        return parse_meta(bytes);
    }

    // Fallback: first character
    KeyEvent::new(KeyCode::Char(input.chars().next().unwrap_or('\0')))
}

/// Check if a `KeyEvent` matches a key description string like `"ctrl+c"`,
/// `"enter"`, `"alt+left"`, or `"ctrl+shift+p"`.
///
/// Supported modifiers: `ctrl`, `alt`, `shift`. The `super` modifier is
/// silently ignored since terminals do not send super key events.
///
/// Supported named keys:
/// `escape`/`esc`, `enter`/`return`, `tab`, `space`, `backspace`,
/// `delete`, `insert`, `home`, `end`, `pageup`, `pagedown`,
/// `up`, `down`, `left`, `right`, `f1`-`f24`,
/// `kp_enter`, `kp_0`-`kp_9`, `kp_divide`, `kp_multiply`,
/// `kp_subtract`, `kp_add`, `kp_decimal`.
///
/// Single-character keys (a-z, 0-9, symbols) are matched directly.
pub fn matches_key(event: &KeyEvent, key_id: &str) -> bool {
    let parts: Vec<&str> = key_id.split('+').collect();
    if parts.is_empty() {
        return false;
    }

    let key_name = parts[parts.len() - 1];

    // Extract modifiers from the key_id
    let mut want_ctrl = false;
    let mut want_alt = false;
    let mut want_shift = false;

    for &p in &parts[..parts.len() - 1] {
        match p {
            "ctrl" => want_ctrl = true,
            "alt" => want_alt = true,
            "shift" => want_shift = true,
            "super" => {} // silently ignored
            _ => return false,
        }
    }

    // Verify modifiers match exactly
    if event.modifiers.ctrl != want_ctrl
        || event.modifiers.alt != want_alt
        || event.modifiers.shift != want_shift
    {
        return false;
    }

    // Match key code against key name
    match key_name {
        "escape" | "esc" => event.code == KeyCode::Escape,
        "enter" | "return" => event.code == KeyCode::Enter,
        "tab" => event.code == KeyCode::Tab,
        "backspace" => event.code == KeyCode::Backspace,
        "delete" => event.code == KeyCode::Delete,
        "insert" => event.code == KeyCode::Insert,
        "home" => event.code == KeyCode::Home,
        "end" => event.code == KeyCode::End,
        "pageup" => event.code == KeyCode::PageUp,
        "pagedown" => event.code == KeyCode::PageDown,
        "up" => event.code == KeyCode::Up,
        "down" => event.code == KeyCode::Down,
        "left" => event.code == KeyCode::Left,
        "right" => event.code == KeyCode::Right,
        "space" => event.code == KeyCode::Char(' '),
        // Keypad keys
        "kp_enter" => event.code == KeyCode::KpEnter,
        "kp_0" => event.code == KeyCode::Kp0,
        "kp_1" => event.code == KeyCode::Kp1,
        "kp_2" => event.code == KeyCode::Kp2,
        "kp_3" => event.code == KeyCode::Kp3,
        "kp_4" => event.code == KeyCode::Kp4,
        "kp_5" => event.code == KeyCode::Kp5,
        "kp_6" => event.code == KeyCode::Kp6,
        "kp_7" => event.code == KeyCode::Kp7,
        "kp_8" => event.code == KeyCode::Kp8,
        "kp_9" => event.code == KeyCode::Kp9,
        "kp_divide" => event.code == KeyCode::KpDivide,
        "kp_multiply" => event.code == KeyCode::KpMultiply,
        "kp_subtract" => event.code == KeyCode::KpSubtract,
        "kp_add" => event.code == KeyCode::KpAdd,
        "kp_decimal" => event.code == KeyCode::KpDecimal,
        // Function keys f1-f24
        name if name.starts_with('f') && name.len() > 1 => {
            if let Ok(n) = name[1..].parse::<u8>() {
                event.code == KeyCode::F(n)
            } else {
                false
            }
        }
        // Single character keys
        name if name.len() == 1 => {
            let c = name.chars().next().unwrap();
            if c == '-' && event.code == KeyCode::Char('_') {
                // Special case: "ctrl+-" matches KeyCode::Char('_') with ctrl
                // since Ctrl+_ and Ctrl+- both produce byte 0x1f on US keyboards
                event.code == KeyCode::Char('_')
            } else {
                event.code == KeyCode::Char(c)
            }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Single-byte parsing
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_printable() {
        for c in ' '..='~' {
            let input = c.to_string();
            let key = parse_key(&input);
            assert_eq!(key, KeyEvent::new(KeyCode::Char(c)), "failed for {c:?}");
        }
    }

    #[test]
    fn test_parse_ctrl_letters() {
        // Exclude 0x08 (BS), 0x09 (Tab), 0x0d (CR), 0x1b (ESC)
        // which are handled differently by parse_single_byte.
        for byte in [1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26] {
            let letter = char::from_u32(byte as u32 + 96).unwrap();
            let buf = [byte];
            let input = String::from_utf8_lossy(&buf);
            let key = parse_key(&input);
            let expected = KeyEvent {
                code: KeyCode::Char(letter),
                modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
            };
            assert_eq!(key, expected, "failed for Ctrl+{letter} (0x{byte:02x})");
        }
    }

    #[test]
    fn test_parse_enter() {
        assert_eq!(parse_key("\r"), KeyEvent::new(KeyCode::Enter));
    }

    #[test]
    fn test_parse_tab() {
        assert_eq!(parse_key("\t"), KeyEvent::new(KeyCode::Tab));
    }

    #[test]
    fn test_parse_escape() {
        assert_eq!(parse_key("\x1b"), KeyEvent::new(KeyCode::Escape));
    }

    #[test]
    fn test_parse_backspace() {
        assert_eq!(parse_key("\x7f"), KeyEvent::new(KeyCode::Backspace));
        assert_eq!(parse_key("\x08"), KeyEvent::new(KeyCode::Backspace));
    }

    // ------------------------------------------------------------------
    // Escape sequences
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_arrows() {
        assert_eq!(parse_key("\x1b[A"), KeyEvent::new(KeyCode::Up));
        assert_eq!(parse_key("\x1b[B"), KeyEvent::new(KeyCode::Down));
        assert_eq!(parse_key("\x1b[C"), KeyEvent::new(KeyCode::Right));
        assert_eq!(parse_key("\x1b[D"), KeyEvent::new(KeyCode::Left));
    }

    #[test]
    fn test_parse_home_end() {
        assert_eq!(parse_key("\x1b[H"), KeyEvent::new(KeyCode::Home));
        assert_eq!(parse_key("\x1b[F"), KeyEvent::new(KeyCode::End));
        assert_eq!(parse_key("\x1b[1~"), KeyEvent::new(KeyCode::Home));
        assert_eq!(parse_key("\x1b[4~"), KeyEvent::new(KeyCode::End));
    }

    #[test]
    fn test_parse_insert_delete() {
        assert_eq!(parse_key("\x1b[2~"), KeyEvent::new(KeyCode::Insert));
        assert_eq!(parse_key("\x1b[3~"), KeyEvent::new(KeyCode::Delete));
    }

    #[test]
    fn test_parse_page_up_down() {
        assert_eq!(parse_key("\x1b[5~"), KeyEvent::new(KeyCode::PageUp));
        assert_eq!(parse_key("\x1b[6~"), KeyEvent::new(KeyCode::PageDown));
    }

    #[test]
    fn test_parse_function_keys() {
        assert_eq!(parse_key("\x1bOP"), KeyEvent::new(KeyCode::F(1)));
        assert_eq!(parse_key("\x1bOQ"), KeyEvent::new(KeyCode::F(2)));
        assert_eq!(parse_key("\x1bOR"), KeyEvent::new(KeyCode::F(3)));
        assert_eq!(parse_key("\x1bOS"), KeyEvent::new(KeyCode::F(4)));
        assert_eq!(parse_key("\x1b[15~"), KeyEvent::new(KeyCode::F(5)));
        assert_eq!(parse_key("\x1b[17~"), KeyEvent::new(KeyCode::F(6)));
        assert_eq!(parse_key("\x1b[18~"), KeyEvent::new(KeyCode::F(7)));
        assert_eq!(parse_key("\x1b[19~"), KeyEvent::new(KeyCode::F(8)));
        assert_eq!(parse_key("\x1b[20~"), KeyEvent::new(KeyCode::F(9)));
        assert_eq!(parse_key("\x1b[21~"), KeyEvent::new(KeyCode::F(10)));
        assert_eq!(parse_key("\x1b[23~"), KeyEvent::new(KeyCode::F(11)));
        assert_eq!(parse_key("\x1b[24~"), KeyEvent::new(KeyCode::F(12)));
    }

    #[test]
    fn test_parse_shift_tab() {
        let expected = KeyEvent {
            code: KeyCode::Tab,
            modifiers: KeyModifiers { ctrl: false, alt: false, shift: true },
        };
        assert_eq!(parse_key("\x1b[Z"), expected);
    }

    #[test]
    fn test_parse_bracketed_paste_markers() {
        assert_eq!(parse_key("\x1b[200~"), KeyEvent::new(KeyCode::Char('\u{e000}')));
        assert_eq!(parse_key("\x1b[201~"), KeyEvent::new(KeyCode::Char('\u{e001}')));
    }

    // ------------------------------------------------------------------
    // CSI-u / Kitty sequences
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_csi_u_basic() {
        // \x1b[97u = 'a'
        let key = parse_key("\x1b[97u");
        assert_eq!(key, KeyEvent::new(KeyCode::Char('a')));
    }

    #[test]
    fn test_parse_csi_u_with_modifier() {
        // \x1b[99;5u = Ctrl+c (modifier 5 = ctrl)
        let expected = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        assert_eq!(parse_key("\x1b[99;5u"), expected);
    }

    #[test]
    fn test_parse_csi_u_alt() {
        // \x1b[97;3u = Alt+a (modifier 3 = alt)
        let expected = KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers { ctrl: false, alt: true, shift: false },
        };
        assert_eq!(parse_key("\x1b[97;3u"), expected);
    }

    #[test]
    fn test_parse_csi_u_shift() {
        // \x1b[65;2u = Shift+A (modifier 2 = shift)
        // After shifted letter normalization, A (65) becomes 'a' (97)
        let expected = KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers { ctrl: false, alt: false, shift: true },
        };
        assert_eq!(parse_key("\x1b[65;2u"), expected);
    }

    #[test]
    fn test_parse_csi_u_special_keys() {
        // \x1b[27u = Escape
        assert_eq!(parse_key("\x1b[27u"), KeyEvent::new(KeyCode::Escape));
        // \x1b[9u = Tab
        assert_eq!(parse_key("\x1b[9u"), KeyEvent::new(KeyCode::Tab));
        // \x1b[13u = Enter
        assert_eq!(parse_key("\x1b[13u"), KeyEvent::new(KeyCode::Enter));
        // \x1b[127u = Backspace
        assert_eq!(parse_key("\x1b[127u"), KeyEvent::new(KeyCode::Backspace));
        // \x1b[32u = Space
        assert_eq!(parse_key("\x1b[32u"), KeyEvent::new(KeyCode::Char(' ')));
    }

    // ------------------------------------------------------------------
    // CSI-u functional codepoint normalization (Kitty private range)
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_csi_u_kp_keys() {
        // KP_0 = codepoint 57399
        assert_eq!(parse_key("\x1b[57399u"), KeyEvent::new(KeyCode::Kp0));
        // KP_1 = codepoint 57400
        assert_eq!(parse_key("\x1b[57400u"), KeyEvent::new(KeyCode::Kp1));
        // KP_9 = codepoint 57408
        assert_eq!(parse_key("\x1b[57408u"), KeyEvent::new(KeyCode::Kp9));
        // KP_DECIMAL = codepoint 57409
        assert_eq!(parse_key("\x1b[57409u"), KeyEvent::new(KeyCode::KpDecimal));
        // KP_DIVIDE = codepoint 57410
        assert_eq!(parse_key("\x1b[57410u"), KeyEvent::new(KeyCode::KpDivide));
        // KP_MULTIPLY = codepoint 57411
        assert_eq!(parse_key("\x1b[57411u"), KeyEvent::new(KeyCode::KpMultiply));
        // KP_SUBTRACT = codepoint 57412
        assert_eq!(parse_key("\x1b[57412u"), KeyEvent::new(KeyCode::KpSubtract));
        // KP_ADD = codepoint 57413
        assert_eq!(parse_key("\x1b[57413u"), KeyEvent::new(KeyCode::KpAdd));
        // KP_ENTER = codepoint 57414
        assert_eq!(parse_key("\x1b[57414u"), KeyEvent::new(KeyCode::KpEnter));
    }

    #[test]
    fn test_parse_csi_u_functional_key_codepoints() {
        // Left arrow (57417)
        assert_eq!(parse_key("\x1b[57417u"), KeyEvent::new(KeyCode::Left));
        // Right arrow (57418)
        assert_eq!(parse_key("\x1b[57418u"), KeyEvent::new(KeyCode::Right));
        // Up arrow (57419)
        assert_eq!(parse_key("\x1b[57419u"), KeyEvent::new(KeyCode::Up));
        // Down arrow (57420)
        assert_eq!(parse_key("\x1b[57420u"), KeyEvent::new(KeyCode::Down));
        // PageUp (57421)
        assert_eq!(parse_key("\x1b[57421u"), KeyEvent::new(KeyCode::PageUp));
        // PageDown (57422)
        assert_eq!(parse_key("\x1b[57422u"), KeyEvent::new(KeyCode::PageDown));
        // Home (57423)
        assert_eq!(parse_key("\x1b[57423u"), KeyEvent::new(KeyCode::Home));
        // End (57424)
        assert_eq!(parse_key("\x1b[57424u"), KeyEvent::new(KeyCode::End));
        // Insert (57425)
        assert_eq!(parse_key("\x1b[57425u"), KeyEvent::new(KeyCode::Insert));
        // Delete (57426)
        assert_eq!(parse_key("\x1b[57426u"), KeyEvent::new(KeyCode::Delete));
    }

    // ------------------------------------------------------------------
    // CSI-u base layout key fallback
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_csi_u_base_layout_key_fallback() {
        // Cyrillic 'С' (codepoint 1083) with base layout 'c' (99), ctrl modifier (5)
        // 1083 is not a Latin letter/digit/symbol → falls back to base layout key 99
        let expected = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        assert_eq!(parse_key("\x1b[1083::99;5u"), expected);
    }

    #[test]
    fn test_parse_csi_u_base_layout_key_known_no_fallback() {
        // 'd' (100) with base layout 'x' (120), shift modifier (2)
        // 100 is a known Latin letter → NO fallback to base layout
        let expected = KeyEvent {
            code: KeyCode::Char('d'),
            modifiers: KeyModifiers { ctrl: false, alt: false, shift: true },
        };
        assert_eq!(parse_key("\x1b[100::120;2u"), expected);
    }

    #[test]
    fn test_parse_csi_u_shifted_key_no_effect_on_matching() {
        // 'a' (97) with shifted 'A' (65), shift modifier (2)
        // codepoint 97 is known, no base layout fallback
        // shift + 'a' → Char('a') with shift (no normalization needed for lowercase)
        let expected = KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers { ctrl: false, alt: false, shift: true },
        };
        assert_eq!(parse_key("\x1b[97:65;2u"), expected);
    }

    // ------------------------------------------------------------------
    // CSI-u event type parsing
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_csi_u_event_type_default_is_press() {
        parse_key("\x1b[97u");
        assert_eq!(last_event_type(), KeyEventType::Press);
    }

    #[test]
    fn test_parse_csi_u_event_type_repeat() {
        // \x1b[97;1:2u = 'a' with event type 2 (repeat)
        parse_key("\x1b[97;1:2u");
        assert_eq!(last_event_type(), KeyEventType::Repeat);
    }

    #[test]
    fn test_parse_csi_u_event_type_release() {
        // \x1b[97;1:3u = 'a' with event type 3 (release)
        parse_key("\x1b[97;1:3u");
        assert_eq!(last_event_type(), KeyEventType::Release);
    }

    #[test]
    fn test_parse_csi_u_event_type_press_explicit() {
        // \x1b[97;1:1u = 'a' with event type 1 (press)
        parse_key("\x1b[97;1:1u");
        assert_eq!(last_event_type(), KeyEventType::Press);
    }

    // ------------------------------------------------------------------
    // F13-F24 support
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_f13_to_f24_tilde() {
        assert_eq!(parse_key("\x1b[25~"), KeyEvent::new(KeyCode::F(13)));
        assert_eq!(parse_key("\x1b[26~"), KeyEvent::new(KeyCode::F(14)));
        assert_eq!(parse_key("\x1b[27~"), KeyEvent::new(KeyCode::F(15)));
        assert_eq!(parse_key("\x1b[28~"), KeyEvent::new(KeyCode::F(16)));
        assert_eq!(parse_key("\x1b[29~"), KeyEvent::new(KeyCode::F(17)));
        assert_eq!(parse_key("\x1b[30~"), KeyEvent::new(KeyCode::F(18)));
        assert_eq!(parse_key("\x1b[31~"), KeyEvent::new(KeyCode::F(19)));
        assert_eq!(parse_key("\x1b[32~"), KeyEvent::new(KeyCode::F(20)));
        assert_eq!(parse_key("\x1b[33~"), KeyEvent::new(KeyCode::F(21)));
        assert_eq!(parse_key("\x1b[34~"), KeyEvent::new(KeyCode::F(22)));
        assert_eq!(parse_key("\x1b[35~"), KeyEvent::new(KeyCode::F(23)));
        assert_eq!(parse_key("\x1b[36~"), KeyEvent::new(KeyCode::F(24)));
    }

    #[test]
    fn test_parse_f13_to_f24_modified() {
        // F13 with shift modifier (2)
        let expected = KeyEvent {
            code: KeyCode::F(13),
            modifiers: KeyModifiers { ctrl: false, alt: false, shift: true },
        };
        assert_eq!(parse_key("\x1b[25;2~"), expected);

        // F20 with ctrl modifier (5)
        let expected = KeyEvent {
            code: KeyCode::F(20),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        assert_eq!(parse_key("\x1b[32;5~"), expected);
    }

    // ------------------------------------------------------------------
    // Modified CSI keys
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_csi_modified_arrow() {
        // \x1b[1;5A = Ctrl+Up (modifier 5 = ctrl)
        let expected = KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        assert_eq!(parse_key("\x1b[1;5A"), expected);
    }

    #[test]
    fn test_parse_csi_modified_home() {
        // \x1b[1;3H = Alt+Home
        let expected = KeyEvent {
            code: KeyCode::Home,
            modifiers: KeyModifiers { ctrl: false, alt: true, shift: false },
        };
        assert_eq!(parse_key("\x1b[1;3H"), expected);
    }

    // ------------------------------------------------------------------
    // Legacy modifyOtherKeys
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_modify_other_keys_ctrl_c() {
        // \x1b[27;5;99~ = Ctrl+c (modifier 5 = ctrl)
        let expected = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        assert_eq!(parse_key("\x1b[27;5;99~"), expected);
    }

    #[test]
    fn test_parse_modify_other_keys_shift_a() {
        // \x1b[27;2;65~ = Shift+A (modifier 2 = shift, code 65 = 'A')
        let expected = KeyEvent {
            code: KeyCode::Char('A'),
            modifiers: KeyModifiers { ctrl: false, alt: false, shift: true },
        };
        assert_eq!(parse_key("\x1b[27;2;65~"), expected);
    }

    #[test]
    fn test_parse_modify_other_keys_escape() {
        // \x1b[27;1;27~ = Escape (modifier 1 = none, code 27 = ESC)
        assert_eq!(parse_key("\x1b[27;1;27~"), KeyEvent::new(KeyCode::Escape));
    }

    #[test]
    fn test_parse_modify_other_keys_enter() {
        // \x1b[27;1;13~ = Enter (modifier 1 = none, code 13 = CR)
        assert_eq!(parse_key("\x1b[27;1;13~"), KeyEvent::new(KeyCode::Enter));
    }

    // ------------------------------------------------------------------
    // Modified CSI event type
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_csi_modified_arrow_event_type() {
        // \x1b[1;5:2A = Ctrl+Up with event type 2 (repeat)
        parse_key("\x1b[1;5:2A");
        assert_eq!(last_event_type(), KeyEventType::Repeat);
    }

    #[test]
    fn test_parse_csi_modified_tilde_event_type() {
        // \x1b[3;5:3~ = Ctrl+Delete with event type 3 (release)
        parse_key("\x1b[3;5:3~");
        assert_eq!(last_event_type(), KeyEventType::Release);
    }

    // ------------------------------------------------------------------
    // Meta / Alt sequences
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_alt_letter() {
        let expected = KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers { ctrl: false, alt: true, shift: false },
        };
        assert_eq!(parse_key("\x1ba"), expected);
    }

    #[test]
    fn test_parse_alt_enter() {
        let expected = KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers { ctrl: false, alt: true, shift: false },
        };
        assert_eq!(parse_key("\x1b\r"), expected);
    }

    #[test]
    fn test_parse_alt_backspace() {
        let expected = KeyEvent {
            code: KeyCode::Backspace,
            modifiers: KeyModifiers { ctrl: false, alt: true, shift: false },
        };
        assert_eq!(parse_key("\x1b\x7f"), expected);
    }

    // ------------------------------------------------------------------
    // matches_key
    // ------------------------------------------------------------------

    #[test]
    fn test_matches_simple_char() {
        let event = KeyEvent::new(KeyCode::Char('a'));
        assert!(matches_key(&event, "a"));
        assert!(!matches_key(&event, "b"));
        assert!(!matches_key(&event, "ctrl+a"));
    }

    #[test]
    fn test_matches_ctrl_char() {
        let event = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        assert!(matches_key(&event, "ctrl+c"));
        assert!(!matches_key(&event, "c"));
        assert!(!matches_key(&event, "alt+c"));
    }

    #[test]
    fn test_matches_named_keys() {
        assert!(matches_key(&KeyEvent::new(KeyCode::Enter), "enter"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Enter), "return"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Escape), "escape"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Escape), "esc"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Tab), "tab"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Backspace), "backspace"));
    }

    #[test]
    fn test_matches_arrows() {
        assert!(matches_key(&KeyEvent::new(KeyCode::Up), "up"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Down), "down"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Left), "left"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Right), "right"));
    }

    #[test]
    fn test_matches_function_keys() {
        assert!(matches_key(&KeyEvent::new(KeyCode::F(1)), "f1"));
        assert!(matches_key(&KeyEvent::new(KeyCode::F(12)), "f12"));
        assert!(matches_key(&KeyEvent::new(KeyCode::F(13)), "f13"));
        assert!(matches_key(&KeyEvent::new(KeyCode::F(24)), "f24"));
        assert!(!matches_key(&KeyEvent::new(KeyCode::F(1)), "f2"));
    }

    #[test]
    fn test_matches_shift_tab() {
        let event = KeyEvent {
            code: KeyCode::Tab,
            modifiers: KeyModifiers { ctrl: false, alt: false, shift: true },
        };
        assert!(matches_key(&event, "shift+tab"));
        assert!(!matches_key(&event, "tab"));
    }

    #[test]
    fn test_matches_alt_arrow() {
        let event = KeyEvent {
            code: KeyCode::Left,
            modifiers: KeyModifiers { ctrl: false, alt: true, shift: false },
        };
        assert!(matches_key(&event, "alt+left"));
        assert!(!matches_key(&event, "left"));
    }

    #[test]
    fn test_matches_ctrl_arrow() {
        let event = KeyEvent {
            code: KeyCode::Right,
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        assert!(matches_key(&event, "ctrl+right"));
    }

    #[test]
    fn test_matches_ctrl_hyphen() {
        // Ctrl+- should match KeyCode::Char('_') with ctrl
        let event = KeyEvent {
            code: KeyCode::Char('_'),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        assert!(matches_key(&event, "ctrl+-"));
    }

    #[test]
    fn test_matches_alt_letter() {
        let event = KeyEvent {
            code: KeyCode::Char('b'),
            modifiers: KeyModifiers { ctrl: false, alt: true, shift: false },
        };
        assert!(matches_key(&event, "alt+b"));
        assert!(!matches_key(&event, "b"));
        assert!(!matches_key(&event, "ctrl+b"));
    }

    #[test]
    fn test_matches_shift_enter() {
        let event = KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers { ctrl: false, alt: false, shift: true },
        };
        assert!(matches_key(&event, "shift+enter"));
    }

    #[test]
    fn test_matches_compound_modifiers() {
        let event = KeyEvent {
            code: KeyCode::Char(']'),
            modifiers: KeyModifiers { ctrl: true, alt: true, shift: false },
        };
        assert!(matches_key(&event, "ctrl+alt+]"));
        assert!(!matches_key(&event, "ctrl+]"));
        assert!(!matches_key(&event, "alt+]"));
    }

    #[test]
    fn test_matches_space() {
        assert!(matches_key(&KeyEvent::new(KeyCode::Char(' ')), "space"));
        assert!(!matches_key(&KeyEvent::new(KeyCode::Char(' ')), "ctrl+space"));

        let ctrl_space = KeyEvent {
            code: KeyCode::Char(' '),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        assert!(matches_key(&ctrl_space, "ctrl+space"));
    }

    #[test]
    fn test_matches_home_end() {
        assert!(matches_key(&KeyEvent::new(KeyCode::Home), "home"));
        assert!(matches_key(&KeyEvent::new(KeyCode::End), "end"));
    }

    #[test]
    fn test_matches_page_keys() {
        assert!(matches_key(&KeyEvent::new(KeyCode::PageUp), "pageup"));
        assert!(matches_key(&KeyEvent::new(KeyCode::PageDown), "pagedown"));
    }

    #[test]
    fn test_matches_delete_insert() {
        assert!(matches_key(&KeyEvent::new(KeyCode::Delete), "delete"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Insert), "insert"));
    }

    #[test]
    fn test_matches_super_ignored() {
        let event = KeyEvent::new(KeyCode::Char('k'));
        // super is ignored in matching
        assert!(matches_key(&event, "super+k"));
    }

    // ------------------------------------------------------------------
    // matches_key for Kp variants
    // ------------------------------------------------------------------

    #[test]
    fn test_matches_kp_keys() {
        assert!(matches_key(&KeyEvent::new(KeyCode::KpEnter), "kp_enter"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Kp0), "kp_0"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Kp1), "kp_1"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Kp2), "kp_2"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Kp3), "kp_3"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Kp4), "kp_4"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Kp5), "kp_5"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Kp6), "kp_6"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Kp7), "kp_7"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Kp8), "kp_8"));
        assert!(matches_key(&KeyEvent::new(KeyCode::Kp9), "kp_9"));
        assert!(matches_key(&KeyEvent::new(KeyCode::KpDivide), "kp_divide"));
        assert!(matches_key(&KeyEvent::new(KeyCode::KpMultiply), "kp_multiply"));
        assert!(matches_key(&KeyEvent::new(KeyCode::KpSubtract), "kp_subtract"));
        assert!(matches_key(&KeyEvent::new(KeyCode::KpAdd), "kp_add"));
        assert!(matches_key(&KeyEvent::new(KeyCode::KpDecimal), "kp_decimal"));
    }

    #[test]
    fn test_matches_kp_with_modifiers() {
        let event = KeyEvent {
            code: KeyCode::KpEnter,
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        assert!(matches_key(&event, "ctrl+kp_enter"));
        assert!(!matches_key(&event, "kp_enter"));
    }

    // ------------------------------------------------------------------
    // Parsing with event type — verify event is still correctly decoded
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_csi_u_with_event_type_preserves_key() {
        // Verify that parsing a CSI-u sequence with event type still returns
        // the correct KeyEvent
        let key = parse_key("\x1b[99;5:2u");
        let expected = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        assert_eq!(key, expected);
    }

    // ------------------------------------------------------------------
    // Display formatting
    // ------------------------------------------------------------------

    #[test]
    fn test_display_kp_keys() {
        assert_eq!(KeyEvent::new(KeyCode::KpEnter).to_string(), "kp_enter");
        assert_eq!(KeyEvent::new(KeyCode::Kp0).to_string(), "kp_0");
        assert_eq!(KeyEvent::new(KeyCode::KpDivide).to_string(), "kp_divide");
        assert_eq!(KeyEvent::new(KeyCode::KpMultiply).to_string(), "kp_multiply");
        assert_eq!(KeyEvent::new(KeyCode::KpSubtract).to_string(), "kp_subtract");
        assert_eq!(KeyEvent::new(KeyCode::KpAdd).to_string(), "kp_add");
        assert_eq!(KeyEvent::new(KeyCode::KpDecimal).to_string(), "kp_decimal");
    }

    #[test]
    fn test_display_kp_with_modifier() {
        let event = KeyEvent {
            code: KeyCode::KpEnter,
            modifiers: KeyModifiers { ctrl: false, alt: true, shift: false },
        };
        assert_eq!(event.to_string(), "alt+kp_enter");
    }

    // ------------------------------------------------------------------
    // Edge cases
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_modify_other_keys_invalid_not_27() {
        // \x1b[28;5;99~ is NOT a modifyOtherKeys sequence (starts with 28, not 27)
        // Should fall through to parse_csi_modified and return None
        // Currently falls through to fallback Char('c') with no modifiers
        let key = parse_key("\x1b[28;5;99~");
        // payload "28;5;99~" — starts with "28;5;99" ending in '~'
        // Not in fixed CSI, not CSI-u, modifyOtherKeys needs "27;...", so falls through
        // parse_csi_modified: rfind(';') splits to "28;5" and "99" → num="28;5" parse fails → None
        // Fallback: nth(2) = input[2] = '2' (from "2" in "28")
        assert_eq!(key.code, KeyCode::Char('2'));
    }

    #[test]
    fn test_parse_csi_u_with_shifted_and_base() {
        // Full format: \x1b[1083:1043:99;5:2u
        // codepoint=1083 (Cyrillic), shifted=1043, base=99 ('c'), modifier=5 (ctrl), event=2 (repeat)
        // 1083 is not known → fall back to base=99 → Char('c') with ctrl
        let expected = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        let key = parse_key("\x1b[1083:1043:99;5:2u");
        assert_eq!(key, expected);
        // Event type should be repeat
        assert_eq!(last_event_type(), KeyEventType::Repeat);
    }

    #[test]
    fn test_parse_csi_u_codepoint_57415_and_57416() {
        // KP_EQUAL (57415) → '=' (not a Kp variant, just Char('='))
        assert_eq!(parse_key("\x1b[57415u"), KeyEvent::new(KeyCode::Char('=')));
        // KP_SEPARATOR (57416) → ',' (not a Kp variant, just Char(','))
        assert_eq!(parse_key("\x1b[57416u"), KeyEvent::new(KeyCode::Char(',')));
    }

    #[test]
    fn test_parse_shifted_csi_u_with_modifier() {
        // \x1b[97:65;5u = Ctrl+'a' with shifted 'A', modifier 5 (ctrl)
        // codepoint 97 is known, so no base layout fallback
        let expected = KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers { ctrl: true, alt: false, shift: false },
        };
        assert_eq!(parse_key("\x1b[97:65;5u"), expected);
    }
}
