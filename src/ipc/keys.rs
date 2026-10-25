//! Key specs for `rustune key` / `{"keys":[...]}`: `j`, `Enter`, `ctrl+k`,
//! `shift+Right`. They become the same `KeyEvent`s a terminal reports.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Named keys (case-insensitive), as listed in `--help`.
const NAMES: &[(&str, KeyCode)] = &[
    ("enter", KeyCode::Enter),
    ("return", KeyCode::Enter),
    ("esc", KeyCode::Esc),
    ("escape", KeyCode::Esc),
    ("tab", KeyCode::Tab),
    ("backtab", KeyCode::BackTab),
    ("backspace", KeyCode::Backspace),
    ("bs", KeyCode::Backspace),
    ("up", KeyCode::Up),
    ("down", KeyCode::Down),
    ("left", KeyCode::Left),
    ("right", KeyCode::Right),
    ("pgup", KeyCode::PageUp),
    ("pageup", KeyCode::PageUp),
    ("pgdn", KeyCode::PageDown),
    ("pagedown", KeyCode::PageDown),
    ("home", KeyCode::Home),
    ("end", KeyCode::End),
    ("space", KeyCode::Char(' ')),
    ("del", KeyCode::Delete),
    ("delete", KeyCode::Delete),
    ("ins", KeyCode::Insert),
    ("insert", KeyCode::Insert),
];

fn modifier(name: &str) -> Option<KeyModifiers> {
    match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some(KeyModifiers::CONTROL),
        "alt" | "meta" | "option" => Some(KeyModifiers::ALT),
        "shift" => Some(KeyModifiers::SHIFT),
        _ => None,
    }
}

fn key_code(name: &str) -> Option<KeyCode> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(KeyCode::Char(c));
    }
    let lower = name.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return (1..=12).contains(&n).then_some(KeyCode::F(n));
    }
    NAMES
        .iter()
        .find(|(n, _)| *n == lower)
        .map(|(_, code)| *code)
}

/// Parse one key spec into the event a terminal would send for it.
pub fn parse_key(spec: &str) -> Result<KeyEvent, String> {
    let bad = |why: &str| {
        format!(
            "bad key '{spec}': {why} (use a character, a name like Enter, Esc, Tab, Up, \
             PgDn, Space, F1, or a modifier like ctrl+k, alt+x, shift+Right)"
        )
    };
    if spec.is_empty() {
        return Err(bad("empty"));
    }
    // `+` itself, alone or after modifiers (`ctrl++`).
    let (mods_part, key_part) = if spec == "+" {
        ("", "+")
    } else if let Some(mods) = spec.strip_suffix("++") {
        (mods, "+")
    } else {
        match spec.rsplit_once('+') {
            Some((mods, key)) if !mods.is_empty() => (mods, key),
            _ => ("", spec),
        }
    };
    let mut mods = KeyModifiers::NONE;
    if !mods_part.is_empty() {
        for name in mods_part.split('+') {
            mods |= modifier(name).ok_or_else(|| bad(&format!("unknown modifier '{name}'")))?;
        }
    }
    let mut code = key_code(key_part).ok_or_else(|| bad("unknown key"))?;
    match code {
        // Terminals report an upper-case letter with Shift, and Shift+Tab
        // as BackTab.
        KeyCode::Char(c) if c.is_uppercase() => mods |= KeyModifiers::SHIFT,
        KeyCode::Char(c) if mods.contains(KeyModifiers::SHIFT) && c.is_lowercase() => {
            let mut upper = c.to_uppercase();
            if let (Some(u), None) = (upper.next(), upper.next()) {
                code = KeyCode::Char(u);
            }
        }
        KeyCode::Tab if mods.contains(KeyModifiers::SHIFT) => code = KeyCode::BackTab,
        KeyCode::BackTab => mods |= KeyModifiers::SHIFT,
        _ => {}
    }
    Ok(KeyEvent::new(code, mods))
}

/// The key specs that type `text` (one per character; space is `Space`).
pub fn text_keys(text: &str) -> Vec<String> {
    text.chars()
        .map(|c| match c {
            ' ' => "Space".to_string(),
            '\n' => "Enter".to_string(),
            '\t' => "Tab".to_string(),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> Result<KeyEvent, String> {
        Ok(KeyEvent::new(code, mods))
    }

    #[test]
    fn characters_and_names() {
        let none = KeyModifiers::NONE;
        assert_eq!(parse_key("j"), key(KeyCode::Char('j'), none));
        assert_eq!(parse_key("?"), key(KeyCode::Char('?'), none));
        assert_eq!(parse_key("+"), key(KeyCode::Char('+'), none));
        assert_eq!(parse_key("é"), key(KeyCode::Char('é'), none));
        assert_eq!(parse_key("G"), key(KeyCode::Char('G'), KeyModifiers::SHIFT));
        for (spec, code) in [
            ("Enter", KeyCode::Enter),
            ("esc", KeyCode::Esc),
            ("Tab", KeyCode::Tab),
            ("Backspace", KeyCode::Backspace),
            ("Up", KeyCode::Up),
            ("Down", KeyCode::Down),
            ("Left", KeyCode::Left),
            ("Right", KeyCode::Right),
            ("PgUp", KeyCode::PageUp),
            ("PgDn", KeyCode::PageDown),
            ("Home", KeyCode::Home),
            ("End", KeyCode::End),
            ("Space", KeyCode::Char(' ')),
            ("Del", KeyCode::Delete),
            ("F5", KeyCode::F(5)),
        ] {
            assert_eq!(parse_key(spec), key(code, none), "{spec}");
        }
        assert_eq!(
            parse_key("BackTab"),
            key(KeyCode::BackTab, KeyModifiers::SHIFT)
        );
    }

    #[test]
    fn modifiers() {
        assert_eq!(
            parse_key("ctrl+k"),
            key(KeyCode::Char('k'), KeyModifiers::CONTROL)
        );
        assert_eq!(
            parse_key("Ctrl+Alt+x"),
            key(
                KeyCode::Char('x'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )
        );
        assert_eq!(
            parse_key("shift+Right"),
            key(KeyCode::Right, KeyModifiers::SHIFT)
        );
        assert_eq!(
            parse_key("shift+s"),
            key(KeyCode::Char('S'), KeyModifiers::SHIFT)
        );
        assert_eq!(
            parse_key("shift+Tab"),
            key(KeyCode::BackTab, KeyModifiers::SHIFT)
        );
        assert_eq!(
            parse_key("ctrl++"),
            key(KeyCode::Char('+'), KeyModifiers::CONTROL)
        );
    }

    #[test]
    fn invalid_keys_are_explained() {
        for bad in ["", "Enterr", "hyper+k", "ctrl+", "F13", "ab"] {
            let err = parse_key(bad).unwrap_err();
            assert!(err.starts_with("bad key"), "{bad}: {err}");
        }
        assert!(parse_key("hyper+k")
            .unwrap_err()
            .contains("modifier 'hyper'"));
    }

    #[test]
    fn text_becomes_keys() {
        assert_eq!(text_keys("a b"), vec!["a", "Space", "b"]);
        assert!(text_keys("Hi there!")
            .iter()
            .all(|spec| parse_key(spec).is_ok()));
    }

    #[test]
    fn keymap_sees_injected_keys_like_typed_ones() {
        use crate::keymap::{lookup, Action, Ctx};
        let ctx = Ctx::GLOBAL.or(Ctx::LIBRARY);
        for spec in ["?", "ctrl+k", "ctrl+K"] {
            let key = parse_key(spec).unwrap();
            assert_eq!(lookup(ctx, &key), Some(Action::Palette), "{spec}");
        }
        let key = parse_key("BackTab").unwrap();
        assert_eq!(lookup(ctx, &key), Some(Action::PrevTab));
    }
}
