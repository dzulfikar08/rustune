//! Command palette (`?` / Ctrl+K): every action valid in the current view
//! with its keys, a description and the current value of toggles, filtered
//! by a small fuzzy matcher as you type. Built from the key table, so it
//! lists exactly what the keys do; Enter runs the action like its key.

use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, BorderType, Clear, Widget};
use ratatui::Frame;

use crate::app::App;
use crate::keymap::{self, Action, Ctx, BINDINGS};
use crate::view::{PlayStatus, RepeatMode};
use crate::winamp::{VisMode as WinampVis, WinampUiState};

/// Longest query and text the matcher looks at (stack buffers).
const MAX_QUERY: usize = 32;
const MAX_TEXT: usize = 128;

/// Per matched character.
const MATCH: i32 = 16;
/// Match on the first character of the text.
const START: i32 = 32;
/// Match on the first character of a word (or a lower→upper case change).
const BOUNDARY: i32 = 24;
/// Match right after the previous matched character.
const CONSECUTIVE: i32 = 24;
/// Characters skipped before the first match cost 1 each, up to this.
const LEAD_CAP: i32 = 15;
/// An entry whose key is exactly the typed character ranks first.
const EXACT_KEY: i32 = i32::MAX / 2;
const NO_MATCH: i32 = i32::MIN / 4;

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Case-insensitive fuzzy score of `query` against `text`, or `None` when
/// the query (whitespace ignored) is not a subsequence of the text. Higher
/// is better: word starts and runs of consecutive characters earn bonuses,
/// skipped characters cost a point each. Allocation-free.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let mut q = ['\0'; MAX_QUERY];
    let mut m = 0;
    for (i, c) in query
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(MAX_QUERY)
        .enumerate()
    {
        q[i] = fold(c);
        m = i + 1;
    }
    if m == 0 {
        return Some(0);
    }
    let mut t = ['\0'; MAX_TEXT];
    let mut bonus = [0; MAX_TEXT];
    let mut n = 0;
    let mut prev: Option<char> = None;
    for (j, c) in text.chars().take(MAX_TEXT).enumerate() {
        bonus[j] = match prev {
            None => START,
            Some(p) if !p.is_alphanumeric() || (p.is_lowercase() && c.is_uppercase()) => BOUNDARY,
            Some(_) => 0,
        };
        t[j] = fold(c);
        prev = Some(c);
        n = j + 1;
    }
    if m > n {
        return None;
    }
    // `prev_row[j]`: best score with the previous query character at `t[j]`.
    let mut prev_row = [NO_MATCH; MAX_TEXT];
    let mut row = [NO_MATCH; MAX_TEXT];
    for (i, &qc) in q[..m].iter().enumerate() {
        // Best `prev_row[k] + k` over k <= j - 2: a gap of j - k - 1 chars.
        let mut gap_best = NO_MATCH;
        for j in 0..n {
            if j >= 2 {
                gap_best = gap_best.max(prev_row[j - 2] + (j - 2) as i32);
            }
            row[j] = if t[j] == qc {
                let from = if i == 0 {
                    -(j as i32).min(LEAD_CAP)
                } else if j == 0 {
                    NO_MATCH
                } else {
                    (prev_row[j - 1] + CONSECUTIVE).max(gap_best - j as i32 + 1)
                };
                if from > NO_MATCH / 2 {
                    from + MATCH + bonus[j]
                } else {
                    NO_MATCH
                }
            } else {
                NO_MATCH
            };
        }
        std::mem::swap(&mut prev_row, &mut row);
    }
    let best = prev_row[..n].iter().copied().max().unwrap_or(NO_MATCH);
    (best > NO_MATCH / 2).then_some(best)
}

pub enum PaletteOutcome {
    Stay,
    Close,
    Run(Action),
}

pub struct Palette {
    query: String,
    /// Indices into `BINDINGS` valid in the view that opened the palette.
    entries: Vec<usize>,
    /// Entries matching `query`, best first.
    shown: Vec<usize>,
    /// Scratch for ranking, kept to reuse its allocation.
    scored: Vec<(i32, usize)>,
    cursor: usize,
}

impl Palette {
    pub fn new(ctx: Ctx) -> Self {
        let entries = BINDINGS
            .iter()
            .enumerate()
            .filter(|(_, b)| b.ctx.intersects(ctx) && b.action != Action::Palette)
            .map(|(i, _)| i)
            .collect();
        let mut palette = Self {
            query: String::new(),
            entries,
            shown: Vec::new(),
            scored: Vec::new(),
            cursor: 0,
        };
        palette.refilter();
        palette
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> PaletteOutcome {
        match keymap::lookup(Ctx::PALETTE, key) {
            Some(Action::PaletteClose) => return PaletteOutcome::Close,
            Some(Action::PaletteRun) => {
                return self
                    .selected()
                    .map_or(PaletteOutcome::Stay, PaletteOutcome::Run)
            }
            Some(Action::PaletteUp) => self.step(false),
            Some(Action::PaletteDown) => self.step(true),
            Some(Action::DeleteBack) => {
                if self.query.pop().is_some() {
                    self.refilter();
                }
            }
            Some(Action::ClearLine) => {
                if !self.query.is_empty() {
                    self.query.clear();
                    self.refilter();
                }
            }
            Some(_) => {}
            None => {
                if let KeyCode::Char(c) = key.code {
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    {
                        self.query.push(c);
                        self.refilter();
                    }
                }
            }
        }
        PaletteOutcome::Stay
    }

    /// Action under the cursor.
    pub fn selected(&self) -> Option<Action> {
        self.shown.get(self.cursor).map(|&i| BINDINGS[i].action)
    }

    fn step(&mut self, down: bool) {
        let n = self.shown.len();
        if n > 0 {
            self.cursor = if down {
                (self.cursor + 1) % n
            } else {
                (self.cursor + n - 1) % n
            };
        }
    }

    fn refilter(&mut self) {
        let mut chars = self.query.chars();
        let single = match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        };
        self.scored.clear();
        for &i in &self.entries {
            let b = &BINDINGS[i];
            let score = if single.is_some_and(|c| b.keys.iter().any(|k| k.is_char(c))) {
                Some(EXACT_KEY)
            } else {
                fuzzy_score(&self.query, b.desc)
            };
            if let Some(score) = score {
                self.scored.push((score, i));
            }
        }
        // Stable: equal scores keep the table order.
        self.scored.sort_by(|a, b| b.0.cmp(&a.0));
        self.shown.clear();
        self.shown.extend(self.scored.iter().map(|&(_, i)| i));
        self.cursor = 0;
    }
}

/// Current value shown next to toggles and cycles.
fn value<'a>(action: Action, app: &'a App, winamp: &WinampUiState) -> Option<Cow<'a, str>> {
    let on_off = |on: bool| Cow::Borrowed(if on { "on" } else { "off" });
    let skin = app.is_winamp();
    Some(match action {
        Action::TogglePause => Cow::Borrowed(match app.play_status() {
            PlayStatus::Playing => "playing",
            PlayStatus::Paused => "paused",
            PlayStatus::Stopped => "stopped",
        }),
        Action::Shuffle => on_off(app.queue.shuffle()),
        Action::Repeat => Cow::Borrowed(match app.queue.repeat() {
            RepeatMode::Off => "off",
            RepeatMode::All => "all",
            RepeatMode::One => "one",
        }),
        Action::VolumeUp | Action::VolumeDown => Cow::Owned(format!("{:.0}%", app.volume * 100.0)),
        Action::EqToggle => on_off(app.eq.enabled),
        Action::EqPresetPrev | Action::EqPresetNext => {
            Cow::Borrowed(super::eq::current_preset(&app.eq).unwrap_or("custom"))
        }
        Action::EqPanel => on_off(if skin { winamp.show_eq } else { app.show_eq }),
        Action::PlaylistWindow => on_off(if skin {
            winamp.show_playlist
        } else {
            app.show_playlist
        }),
        Action::Lyrics => on_off(app.show_lyrics),
        Action::Visualizer if skin => Cow::Borrowed(match winamp.vis_mode {
            WinampVis::Analyzer => "analyzer",
            WinampVis::Oscilloscope => "scope",
            WinampVis::Off => "off",
        }),
        Action::Visualizer => Cow::Borrowed(app.vis_mode.name()),
        Action::Theme => Cow::Borrowed(app.theme.name.as_str()),
        Action::NextTab | Action::PrevTab => Cow::Borrowed(app.tab.label()),
        _ => return None,
    })
}

/// Columns left from `x` to `right`.
fn room(x: u16, right: u16) -> usize {
    usize::from(right.saturating_sub(x))
}

/// The palette as a centred overlay.
pub fn render(frame: &mut Frame, app: &App, winamp: &WinampUiState) {
    let Some(palette) = &app.palette else {
        return;
    };
    let theme = &app.theme;
    let area = frame.area();
    // Leave the bottom row for the hint bar.
    let space = area.height.saturating_sub(1);
    let wanted = u16::try_from(palette.entries.len())
        .unwrap_or(u16::MAX)
        .saturating_add(3);
    let height = wanted.min(space);
    let width = area.width.min(78);
    if height < 4 || width < 20 {
        return;
    }
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (space - height) / 2,
        width,
        height,
    );
    let buf = frame.buffer_mut();
    Clear.render(rect, buf);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.result_border)
        .title(" Commands ")
        .title_style(theme.page_nav_active);
    let inner = block.inner(rect);
    block.render(rect, buf);
    let right = inner.right();

    // Filter line.
    let (x, _) = buf.set_stringn(
        inner.x,
        inner.y,
        "› ",
        room(inner.x, right),
        theme.input_prompt,
    );
    let (x, _) = buf.set_stringn(x, inner.y, &palette.query, room(x, right), theme.input_text);
    let (x, _) = buf.set_stringn(
        x,
        inner.y,
        &theme.input_cursor,
        room(x, right),
        theme.input_text,
    );
    if palette.query.is_empty() && x < right {
        buf.set_stringn(
            x + 1,
            inner.y,
            "type to filter",
            room(x + 1, right),
            theme.help_desc,
        );
    }

    let list_y = inner.y + 1;
    let visible = usize::from(inner.height.saturating_sub(1));
    if palette.shown.is_empty() {
        buf.set_stringn(
            inner.x + 1,
            list_y,
            "No matching command",
            room(inner.x + 1, right),
            theme.empty_text,
        );
        return;
    }
    let labels = keymap::labels();
    let key_w = palette
        .entries
        .iter()
        .map(|&i| labels[i].chars().count())
        .max()
        .unwrap_or(0)
        .min(14);
    let offset = (palette.cursor + 1).saturating_sub(visible);
    for (row, &i) in palette.shown.iter().skip(offset).take(visible).enumerate() {
        let b = &BINDINGS[i];
        let y = list_y + row as u16;
        let selected = offset + row == palette.cursor;
        let (key_style, desc_style, value_style) = if selected {
            buf.set_style(Rect::new(inner.x, y, inner.width, 1), theme.result_selected);
            (
                theme.result_selected,
                theme.result_selected,
                theme.result_selected,
            )
        } else {
            (theme.help_key, theme.result_title, theme.page_nav_active)
        };
        let key_x = inner.x + 1;
        buf.set_stringn(
            key_x,
            y,
            &labels[i],
            key_w.min(room(key_x, right)),
            key_style,
        );
        let value = value(b.action, app, winamp);
        let value_w = value.as_deref().map_or(0, |v| v.chars().count());
        let value_x = right.saturating_sub(u16::try_from(value_w).unwrap_or(0) + 1);
        let desc_x = key_x.saturating_add(key_w as u16 + 2).min(right);
        buf.set_stringn(
            desc_x,
            y,
            b.desc,
            room(desc_x, value_x.saturating_sub(1)),
            desc_style,
        );
        if let Some(value) = value {
            if value_x > desc_x {
                buf.set_stringn(value_x, y, value, value_w, value_style);
            }
        }
    }
}

/// `keys hint  keys hint …` for `ctx` in one row; stops before a hint that
/// would not fit.
pub fn draw_hints(buf: &mut Buffer, area: Rect, ctx: Ctx, key: Style, desc: Style) {
    if area.height == 0 {
        return;
    }
    let right = area.right();
    let mut x = area.x;
    for (keys, hint) in keymap::hints(ctx) {
        let need = keys.chars().count() + 1 + hint.chars().count();
        if need > room(x, right) {
            break;
        }
        (x, _) = buf.set_stringn(x, area.y, keys, room(x, right), key);
        (x, _) = buf.set_stringn(x + 1, area.y, hint, room(x + 1, right), desc);
        x = x.saturating_add(2).min(right);
    }
}

/// The bottom hint bar of the standard layout.
pub fn render_hints(buf: &mut Buffer, area: Rect, app: &App) {
    let area = Rect {
        x: area.x.saturating_add(1),
        width: area.width.saturating_sub(1),
        ..area
    };
    draw_hints(
        buf,
        area,
        app.hint_ctx(),
        app.theme.help_key,
        app.theme.help_desc,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use ratatui::{backend::TestBackend, Terminal};

    fn score(q: &str, t: &str) -> i32 {
        fuzzy_score(q, t).unwrap_or_else(|| panic!("'{q}' should match '{t}'"))
    }

    fn press(palette: &mut Palette, code: KeyCode) -> PaletteOutcome {
        palette.handle_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn typed(ctx: Ctx, query: &str) -> Palette {
        let mut palette = Palette::new(ctx);
        for c in query.chars() {
            press(&mut palette, KeyCode::Char(c));
        }
        palette
    }

    #[test]
    fn matcher_requires_an_ordered_subsequence() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert_eq!(fuzzy_score("xyz", "Toggle shuffle"), None);
        assert_eq!(fuzzy_score("ba", "ab"), None);
        assert_eq!(fuzzy_score("abc", "ab"), None);
        assert!(
            fuzzy_score("SHUF", "toggle shuffle").is_some(),
            "case-insensitive"
        );
        assert!(
            fuzzy_score("tog shuf", "Toggle shuffle").is_some(),
            "spaces ignored"
        );
    }

    #[test]
    fn word_starts_and_runs_rank_higher() {
        // A word start beats the same letters mid-word.
        assert!(score("ly", "Toggle lyrics panel") > score("ly", "Reply"));
        // The text start beats a later word start.
        assert!(score("sh", "Shuffle") > score("sh", "Toggle shuffle"));
        // A consecutive run beats scattered letters.
        assert!(score("eq", "EQ panel") > score("eq", "Next queue"));
        // Initials of words beat letters buried in one word.
        assert!(score("tp", "Toggle playlist window") > score("tp", "Stop"));
        // Fewer skipped characters win.
        assert!(score("vol", "Volume up") > score("vol", "Visualizer cycle mode, low"));
    }

    #[test]
    fn ranking_uses_descriptions_and_exact_keys() {
        let ctx = Ctx::GLOBAL.or(Ctx::LIBRARY);
        assert_eq!(typed(ctx, "shuffle").selected(), Some(Action::Shuffle));
        assert_eq!(typed(ctx, "lyr").selected(), Some(Action::Lyrics));
        assert_eq!(typed(ctx, "theme").selected(), Some(Action::Theme));
        // A single character that is a key puts that binding first.
        assert_eq!(typed(ctx, "S").selected(), Some(Action::Shuffle));
        assert_eq!(typed(ctx, "t").selected(), Some(Action::Theme));
        assert!(typed(ctx, "zzzz").selected().is_none());
    }

    #[test]
    fn lists_only_actions_of_the_current_view() {
        let has = |p: &Palette, a: Action| p.entries.iter().any(|&i| BINDINGS[i].action == a);
        let library = Palette::new(Ctx::GLOBAL.or(Ctx::LIBRARY));
        assert!(has(&library, Action::Search) && has(&library, Action::Enqueue));
        assert!(!has(&library, Action::Download), "Online only");
        assert!(!has(&library, Action::Remove), "playlist window only");
        assert!(
            !has(&library, Action::Palette),
            "the palette does not list itself"
        );
        assert!(
            has(&library, Action::ClearQueue),
            "palette-only actions are listed"
        );
        let podcasts = Palette::new(Ctx::GLOBAL.or(Ctx::PROVIDER).or(Ctx::PODCAST));
        assert!(has(&podcasts, Action::Subscribe) && has(&podcasts, Action::Back));
        assert!(!has(&podcasts, Action::Search));
    }

    #[test]
    fn keys_filter_move_run_and_close() {
        let mut p = Palette::new(Ctx::GLOBAL.or(Ctx::LIBRARY));
        let first = p.selected();
        press(&mut p, KeyCode::Up);
        assert_ne!(p.selected(), first, "wraps to the last entry");
        press(&mut p, KeyCode::Down);
        assert_eq!(p.selected(), first);
        for c in "quit".chars() {
            press(&mut p, KeyCode::Char(c));
        }
        assert!(matches!(
            press(&mut p, KeyCode::Enter),
            PaletteOutcome::Run(Action::Quit)
        ));
        press(&mut p, KeyCode::Backspace);
        assert_eq!(p.query, "qui");
        p.handle_key(&KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(p.query.is_empty());
        // `?` and `j` are filter text here, not palette / list keys.
        press(&mut p, KeyCode::Char('?'));
        assert_eq!(p.query, "?");
        assert!(matches!(press(&mut p, KeyCode::Esc), PaletteOutcome::Close));
        let ctrl_k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert!(matches!(p.handle_key(&ctrl_k), PaletteOutcome::Close));
    }

    fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    #[test]
    fn renders_keys_descriptions_and_values() {
        let mut app = App::new(Config::default());
        assert!(app.queue.toggle_shuffle());
        app.open_palette();
        if let Some(p) = &mut app.palette {
            for c in "shuffle".chars() {
                press(p, KeyCode::Char(c));
            }
        }
        let winamp = WinampUiState::new();
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| render(f, &app, &winamp)).unwrap();
        let rows: Vec<String> = (0..20).map(|y| row_text(&terminal, y)).collect();
        let hit = rows
            .iter()
            .find(|r| r.contains("Toggle shuffle"))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(
            hit.contains(" S ") && hit.trim_end().ends_with("on │"),
            "{hit}"
        );
        assert!(rows.iter().any(|r| r.contains("› shuffle")));

        // Tiny terminals draw nothing rather than panic.
        let mut tiny = Terminal::new(TestBackend::new(10, 3)).unwrap();
        tiny.draw(|f| render(f, &app, &winamp)).unwrap();
    }

    #[test]
    fn hint_bar_follows_the_view() {
        let mut app = App::new(Config::default());
        let mut terminal = Terminal::new(TestBackend::new(120, 1)).unwrap();
        let mut draw = |app: &App| {
            terminal
                .draw(|f| render_hints(f.buffer_mut(), f.area(), app))
                .unwrap();
            row_text(&terminal, 0)
        };
        let library = draw(&app);
        assert!(
            library.contains("/ search") && library.contains("?/Ctrl+K commands"),
            "{library}"
        );
        app.open_palette();
        let palette = draw(&app);
        assert!(
            palette.contains("Enter run") && palette.contains("Esc/Ctrl+K close"),
            "{palette}"
        );
        app.palette = None;
        app.open_prompt(crate::app::InputPurpose::Search);
        let prompt = draw(&app);
        assert!(
            prompt.contains("Enter submit") && prompt.contains("Esc cancel"),
            "{prompt}"
        );
    }
}
