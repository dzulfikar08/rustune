//! Standard-theme equalizer panel: preamp + 10 band sliders, keyboard driven.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType},
    Frame,
};

use crate::command::Command;
use crate::eq_presets::PRESETS;
use crate::keymap::{Action, Ctx};
use crate::theme::Theme;
use crate::view::{EqSettings, EQ_BANDS, EQ_MAX_DB};

/// Slot 0 = preamp, 1..=EQ_BANDS = bands.
pub const SLOTS: usize = EQ_BANDS + 1;

const LABELS: [&str; SLOTS] = [
    "Pre", "60", "170", "310", "600", "1k", "3k", "6k", "12k", "14k", "16k",
];

/// Label column width (right-aligned) and value column width (`+12.0`).
const LABEL_W: u16 = 3;
const VALUE_W: u16 = 5;

/// Command for an EQ action (panel sliders and the global preset keys).
/// Slider moves change only `focus` (0..SLOTS) and return `None`, as do
/// actions that are not EQ actions.
pub fn handle(action: Action, focus: &mut usize, eq: &EqSettings) -> Option<Command> {
    let slot = (*focus).min(SLOTS - 1);
    let adjust = |delta: f32| Some(set_slot(slot, slot_db(eq, slot) + delta));
    match action {
        Action::EqSlotPrev => {
            *focus = (slot + SLOTS - 1) % SLOTS;
            None
        }
        Action::EqSlotNext => {
            *focus = (slot + 1) % SLOTS;
            None
        }
        Action::EqRaise => adjust(1.0),
        Action::EqLower => adjust(-1.0),
        Action::EqRaiseMore => adjust(3.0),
        Action::EqLowerMore => adjust(-3.0),
        Action::EqReset => Some(set_slot(slot, 0.0)),
        Action::EqPresetPrev => Some(Command::EqPreset(step_preset(eq, false).to_string())),
        Action::EqPresetNext => Some(Command::EqPreset(step_preset(eq, true).to_string())),
        _ => None,
    }
}

/// Name of the preset matching `eq.bands_db` exactly, if any (for the title).
pub fn current_preset(eq: &EqSettings) -> Option<&'static str> {
    PRESETS
        .iter()
        .find(|(_, bands)| *bands == eq.bands_db)
        .map(|(name, _)| *name)
}

pub fn render(frame: &mut Frame, area: Rect, eq: &EqSettings, focus: usize, theme: &Theme) {
    let title = format!(
        " EQ [{}] · {} ",
        if eq.enabled { "on" } else { "off" },
        current_preset(eq).unwrap_or("custom"),
    );
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.result_border)
        .title(title)
        .title_style(theme.page_nav_active);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let rows_total = SLOTS as u16;
    let show_hint = inner.height > rows_total;
    let visible = if show_hint { rows_total } else { inner.height };
    let focus = focus.min(SLOTS - 1);
    // Scroll so the focused row stays visible when rows are clipped.
    let first = (focus + 1).saturating_sub(usize::from(visible));

    let dim = |style: Style| {
        if eq.enabled {
            style
        } else {
            style.add_modifier(Modifier::DIM)
        }
    };
    let filled = dim(theme.gauge_filled);
    let unfilled = dim(theme.gauge_unfilled);
    let slider_w = inner.width.saturating_sub(LABEL_W + VALUE_W + 2);

    for (row, slot) in (first..SLOTS).take(usize::from(visible)).enumerate() {
        let rect = Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
        let focused = slot == focus;
        let text_style = if focused {
            frame.buffer_mut().set_style(rect, theme.result_selected);
            theme.result_selected
        } else {
            theme.result_title
        };
        let db = slot_db(eq, slot);
        let mut spans = Vec::with_capacity(8);
        spans.push(Span::styled(
            format!("{:>w$} ", LABELS[slot], w = usize::from(LABEL_W)),
            text_style,
        ));
        push_slider(&mut spans, slider_w, db, filled, unfilled);
        spans.push(Span::styled(format!(" {}", format_db(db)), text_style));
        frame.render_widget(Line::from(spans), rect);
    }

    if show_hint {
        let rect = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
        super::palette::draw_hints(
            frame.buffer_mut(),
            rect,
            Ctx::EQ,
            theme.help_key,
            theme.help_desc,
        );
    }
}

fn slot_db(eq: &EqSettings, slot: usize) -> f32 {
    match slot {
        0 => eq.preamp_db,
        band => eq.bands_db[band - 1],
    }
}

/// Command setting `slot` to `db`, clamped and snapped to 0.1 dB.
fn set_slot(slot: usize, db: f32) -> Command {
    let db = ((db * 10.0).round() / 10.0).clamp(-EQ_MAX_DB, EQ_MAX_DB);
    match slot {
        0 => Command::SetPreamp(db),
        band => Command::SetEqBand { band: band - 1, db },
    }
}

/// Neighbouring preset name, wrapping; without a match starts at the
/// first (`forward`) or last preset.
fn step_preset(eq: &EqSettings, forward: bool) -> &'static str {
    let n = PRESETS.len();
    let current = current_preset(eq).and_then(|name| PRESETS.iter().position(|(p, _)| *p == name));
    let index = match (current, forward) {
        (Some(i), true) => (i + 1) % n,
        (Some(i), false) => (i + n - 1) % n,
        (None, true) => 0,
        (None, false) => n - 1,
    };
    PRESETS[index].0
}

fn format_db(db: f32) -> String {
    // Avoid "-0.0".
    let db = if db == 0.0 { 0.0 } else { db };
    format!("{db:+.1}")
}

/// Horizontal slider of `width` cells centred on 0 dB, filled from the
/// centre to `db`, with a knob at the value.
fn push_slider(
    spans: &mut Vec<Span<'static>>,
    width: u16,
    db: f32,
    filled: Style,
    unfilled: Style,
) {
    if width < 3 {
        return;
    }
    // Odd width so 0 dB sits exactly in the middle (`center` cells each side).
    let width = (width - 1) | 1;
    let center = i32::from(width / 2);
    let offset = ((db.clamp(-EQ_MAX_DB, EQ_MAX_DB) / EQ_MAX_DB) * center as f32).round() as i32;
    let knob = center + offset;
    let (lo, hi) = (center.min(knob), center.max(knob));

    let mut run = String::new();
    let mut run_style = unfilled;
    for i in 0..i32::from(width) {
        let (ch, style) = if i == knob {
            ('●', filled)
        } else if (lo..=hi).contains(&i) {
            ('━', filled)
        } else if i == center {
            ('┼', unfilled)
        } else {
            ('─', unfilled)
        };
        if style != run_style && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), run_style));
        }
        run_style = style;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_style));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A key press with the panel open, through the shared key table.
    fn handle_key(event: &KeyEvent, focus: &mut usize, eq: &EqSettings) -> Option<Command> {
        match keymap::lookup(Ctx::GLOBAL.or(Ctx::EQ).or(Ctx::LIBRARY), event)? {
            Action::EqToggle => Action::EqToggle.command(),
            action => handle(action, focus, eq),
        }
    }

    fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    #[test]
    fn focus_wraps_both_ways() {
        let eq = EqSettings::default();
        let mut focus = 0;
        assert_eq!(handle_key(&key(KeyCode::Left), &mut focus, &eq), None);
        assert_eq!(focus, SLOTS - 1);
        assert_eq!(handle_key(&key(KeyCode::Char('l')), &mut focus, &eq), None);
        assert_eq!(focus, 0);
        handle_key(&key(KeyCode::Right), &mut focus, &eq);
        handle_key(&key(KeyCode::Char('l')), &mut focus, &eq);
        assert_eq!(focus, 2);
        handle_key(&key(KeyCode::Char('h')), &mut focus, &eq);
        assert_eq!(focus, 1);
        // An out-of-range focus is clamped before moving.
        let mut focus = 99;
        handle_key(&key(KeyCode::Right), &mut focus, &eq);
        assert_eq!(focus, 0);
    }

    #[test]
    fn adjust_targets_slot_and_clamps() {
        let mut eq = EqSettings {
            preamp_db: 11.5,
            ..EqSettings::default()
        };
        eq.bands_db[2] = -11.0;
        let mut focus = 0;
        assert_eq!(
            handle_key(&key(KeyCode::Up), &mut focus, &eq),
            Some(Command::SetPreamp(12.0))
        );
        assert_eq!(
            handle_key(&key(KeyCode::Char('K')), &mut focus, &eq),
            Some(Command::SetPreamp(12.0))
        );
        assert_eq!(
            handle_key(&key(KeyCode::Char('j')), &mut focus, &eq),
            Some(Command::SetPreamp(10.5))
        );
        let mut focus = 3;
        assert_eq!(
            handle_key(&key(KeyCode::Char('J')), &mut focus, &eq),
            Some(Command::SetEqBand { band: 2, db: -12.0 })
        );
        assert_eq!(
            handle_key(&key(KeyCode::Char('k')), &mut focus, &eq),
            Some(Command::SetEqBand { band: 2, db: -10.0 })
        );
        assert_eq!(
            handle_key(&key(KeyCode::Char('0')), &mut focus, &eq),
            Some(Command::SetEqBand { band: 2, db: 0.0 })
        );
        assert_eq!(focus, 3);
    }

    #[test]
    fn toggles_and_ignores_foreign_keys() {
        let eq = EqSettings::default();
        let mut focus = 4;
        assert_eq!(
            handle_key(&key(KeyCode::Char('o')), &mut focus, &eq),
            Some(Command::ToggleEq)
        );
        for code in [
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Char('e'),
            KeyCode::Char('q'),
            KeyCode::Char(' '),
        ] {
            assert_eq!(handle_key(&key(code), &mut focus, &eq), None);
        }
        let ctrl = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&ctrl, &mut focus, &eq), None);
        assert_eq!(focus, 4);
    }

    #[test]
    fn presets_cycle_and_match() {
        let mut eq = EqSettings::default();
        assert_eq!(current_preset(&eq), Some("Flat"));
        let mut focus = 0;
        let next = handle_key(&key(KeyCode::Char(']')), &mut focus, &eq);
        assert_eq!(next, Some(Command::EqPreset(PRESETS[1].0.to_string())));
        let prev = handle_key(&key(KeyCode::Char('[')), &mut focus, &eq);
        let last = PRESETS[PRESETS.len() - 1].0;
        assert_eq!(prev, Some(Command::EqPreset(last.to_string())));

        assert!(crate::eq_presets::apply(&mut eq, last));
        assert_eq!(current_preset(&eq), Some(last));
        let wrapped = handle_key(&key(KeyCode::Char(']')), &mut focus, &eq);
        assert_eq!(wrapped, Some(Command::EqPreset("Flat".to_string())));

        eq.bands_db[0] = 0.5;
        assert_eq!(current_preset(&eq), None);
        assert_eq!(
            handle_key(&key(KeyCode::Char(']')), &mut focus, &eq),
            Some(Command::EqPreset(PRESETS[0].0.to_string()))
        );
        assert_eq!(
            handle_key(&key(KeyCode::Char('[')), &mut focus, &eq),
            Some(Command::EqPreset(last.to_string()))
        );
    }

    #[test]
    fn renders_title_rows_and_hint() {
        let mut eq = EqSettings {
            enabled: true,
            preamp_db: 3.0,
            ..EqSettings::default()
        };
        eq.bands_db[9] = -12.0;
        let theme = Theme::dark();
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        terminal
            .draw(|f| render(f, f.area(), &eq, 1, &theme))
            .unwrap();
        assert!(row_text(&terminal, 0).contains("EQ [on] · custom"));
        let pre = row_text(&terminal, 1);
        assert!(pre.contains("Pre") && pre.contains("+3.0"), "{pre}");
        assert!(row_text(&terminal, 2).contains("+0.0"));
        let last = row_text(&terminal, 11);
        assert!(last.contains("16k") && last.contains("-12.0"), "{last}");
        let hint = row_text(&terminal, 12);
        assert!(
            hint.contains("→/l select") && hint.contains("Esc close"),
            "{hint}"
        );

        // The focused row (slot 1 = 60 Hz) carries the selection style.
        let buf = terminal.backend().buffer();
        assert_eq!(buf[(2, 2)].bg, theme.result_selected.bg.unwrap());

        // The slider knob of a full-cut band sits at the far left.
        let start = last.find("16k ").unwrap() + 4;
        assert_eq!(last[start..].chars().next(), Some('●'));
    }

    #[test]
    fn disabled_title_and_dim_slider() {
        let eq = EqSettings::default();
        let theme = Theme::dark();
        let mut terminal = Terminal::new(TestBackend::new(40, 14)).unwrap();
        terminal
            .draw(|f| render(f, f.area(), &eq, 0, &theme))
            .unwrap();
        assert!(row_text(&terminal, 0).contains("EQ [off] · Flat"));
        let row = row_text(&terminal, 2);
        let knob_x = row.chars().position(|c| c == '●').unwrap() as u16;
        let buf = terminal.backend().buffer();
        assert!(buf[(knob_x, 2)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn clipped_rows_keep_focus_visible() {
        let eq = EqSettings::default();
        let theme = Theme::dark();
        let mut terminal = Terminal::new(TestBackend::new(30, 5)).unwrap();
        terminal
            .draw(|f| render(f, f.area(), &eq, SLOTS - 1, &theme))
            .unwrap();
        assert!(row_text(&terminal, 3).contains("16k"));
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let eq = EqSettings::default();
        let theme = Theme::dark();
        let mut terminal = Terminal::new(TestBackend::new(10, 10)).unwrap();
        for w in 0..=3 {
            for h in 0..=3 {
                terminal
                    .draw(|f| render(f, Rect::new(0, 0, w, h), &eq, 5, &theme))
                    .unwrap();
            }
        }
    }
}
