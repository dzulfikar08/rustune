//! Skin browser state: installed skins and the whole webamp skin museum,
//! with infinite-scroll paging, search and a debounced live preview of the
//! highlighted skin. Network and disk work happen in the controller.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::keymap::{self, Action, Ctx};
use crate::net::{SkinListing, SkinPage};
use crate::skin::WinampSkin;

/// Skins requested per museum page.
pub const PAGE_SIZE: usize = 50;
/// Fetch the next page once the cursor is this close to the end.
const PREFETCH_ROWS: usize = 15;
/// Rows moved by Page Up / Page Down.
const PAGE_ROWS: usize = 10;
/// How long the cursor must rest on a skin before its preview loads.
pub const PREVIEW_DELAY: Duration = Duration::from_millis(250);

/// A single entry in the skin browser.
#[derive(Debug, Clone)]
pub struct SkinEntry {
    /// Museum id; empty for installed skins.
    pub md5: String,
    /// Sanitised file name inside the skins directory.
    pub filename: String,
    pub display_name: String,
    /// Installed in the skins directory.
    pub is_local: bool,
    pub average_color: Option<String>,
}

impl SkinEntry {
    /// Identity for previews: the museum id, or the installed file name.
    pub fn key(&self) -> &str {
        if self.md5.is_empty() {
            &self.filename
        } else {
            &self.md5
        }
    }

    fn preview_source(&self) -> PreviewSource {
        if self.md5.is_empty() {
            PreviewSource::Local(self.filename.clone())
        } else {
            PreviewSource::Museum(self.md5.clone())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Skins installed in the skins directory.
    Local,
    /// The webamp skin museum.
    Online,
}

/// Where a preview is loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewSource {
    /// Museum skin id (downloaded through the preview cache).
    Museum(String),
    /// Installed skin file name.
    Local(String),
}

/// Live preview of the highlighted skin.
#[derive(Default)]
pub enum Preview {
    #[default]
    None,
    /// Waiting for the cursor to rest until `due`.
    Pending {
        key: String,
        due: Instant,
    },
    Loading {
        key: String,
    },
    Ready {
        key: String,
        skin: Box<WinampSkin>,
    },
    Failed {
        key: String,
        error: String,
    },
}

impl Preview {
    fn key(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Pending { key, .. }
            | Self::Loading { key }
            | Self::Ready { key, .. }
            | Self::Failed { key, .. } => Some(key),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SkinBrowserAction {
    None,
    /// Leave the browser.
    Back,
    /// Install the museum skin (from the preview cache) and apply it.
    Install {
        md5: String,
        filename: String,
    },
    /// Apply the installed skin with this file name.
    Apply(String),
    /// Source, listing or search changed: restart the list.
    Reload,
}

pub struct SkinBrowser {
    pub source: Source,
    pub listing: SkinListing,
    pub entries: Vec<SkinEntry>,
    pub list_state: ListState,
    pub loading: bool,
    /// Why the list failed to load (stops the infinite scroll until a reload).
    pub error: Option<String>,
    /// Outcome of the last install or apply.
    pub notice: Option<String>,
    /// Server offset of the next online page (counts hidden NSFW skins).
    pub offset: usize,
    pub has_more: bool,
    /// Size of the listing when known (museum count, installed skins).
    pub total: Option<usize>,
    /// Museum id being installed.
    pub installing: Option<String>,
    /// Applied online search; empty = the listing.
    pub query: String,
    /// Search text being typed.
    pub input: Option<String>,
    pub preview: Preview,
}

impl Default for SkinBrowser {
    fn default() -> Self {
        Self {
            source: Source::Local,
            listing: SkinListing::Museum,
            entries: Vec::new(),
            list_state: ListState::default().with_selected(Some(0)),
            loading: false,
            error: None,
            notice: None,
            offset: 0,
            has_more: false,
            total: None,
            installing: None,
            query: String::new(),
            input: None,
            preview: Preview::None,
        }
    }
}

impl SkinBrowser {
    /// Start browsing `source` from a clean list (the listing choice stays).
    pub fn open(&mut self, source: Source) {
        self.source = source;
        self.query.clear();
        self.input = None;
        self.notice = None;
        self.reset_list();
    }

    /// Free the list and the preview skin when leaving the browser.
    pub fn close(&mut self) {
        self.entries = Vec::new();
        self.input = None;
        self.preview = Preview::None;
        self.loading = false;
    }

    /// Empty the list before (re)loading it.
    pub fn reset_list(&mut self) {
        self.entries.clear();
        self.list_state.select(Some(0));
        *self.list_state.offset_mut() = 0;
        self.offset = 0;
        self.has_more = self.source == Source::Online;
        self.total = None;
        self.error = None;
        self.loading = false;
        self.preview = Preview::None;
    }

    /// Key contexts live in the browser (see `keymap::Ctx`).
    pub fn ctx(&self) -> Ctx {
        if self.input.is_some() {
            Ctx::PROMPT
        } else if self.source == Source::Online {
            Ctx::SKINS.or(Ctx::SKINS_ONLINE)
        } else {
            Ctx::SKINS
        }
    }

    pub fn selected(&self) -> Option<&SkinEntry> {
        self.list_state.selected().and_then(|i| self.entries.get(i))
    }

    fn select(&mut self, i: usize) {
        if !self.entries.is_empty() {
            self.list_state.select(Some(i.min(self.entries.len() - 1)));
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> SkinBrowserAction {
        if let Some(input) = self.input.as_mut() {
            match keymap::lookup(Ctx::PROMPT, key) {
                Some(Action::Submit) => {
                    let text = self.input.take().unwrap_or_default();
                    let text = text.trim();
                    if text == self.query {
                        return SkinBrowserAction::None;
                    }
                    self.query = text.to_string();
                    return SkinBrowserAction::Reload;
                }
                Some(Action::Cancel) => self.input = None,
                Some(Action::DeleteBack) => {
                    input.pop();
                }
                Some(Action::ClearLine) => input.clear(),
                Some(_) => {}
                None => {
                    if let KeyCode::Char(c) = key.code {
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        {
                            input.push(c);
                        }
                    }
                }
            }
            return SkinBrowserAction::None;
        }

        let Some(action) = keymap::lookup(self.ctx(), key) else {
            return SkinBrowserAction::None;
        };
        let cur = self.list_state.selected().unwrap_or(0);
        match action {
            Action::Down => self.select(cur + 1),
            Action::Up => self.select(cur.saturating_sub(1)),
            Action::PageDown => self.select(cur + PAGE_ROWS),
            Action::PageUp => self.select(cur.saturating_sub(PAGE_ROWS)),
            Action::Top => self.select(0),
            Action::Bottom => self.select(usize::MAX),
            Action::Activate => return self.activate(),
            Action::Search => self.input = Some(self.query.clone()),
            Action::SkinFilter => {
                self.listing = match self.listing {
                    SkinListing::Museum => SkinListing::Curated,
                    SkinListing::Curated => SkinListing::Museum,
                };
                self.query.clear();
                return SkinBrowserAction::Reload;
            }
            Action::NextTab => {
                self.source = match self.source {
                    Source::Local => Source::Online,
                    Source::Online => Source::Local,
                };
                self.query.clear();
                return SkinBrowserAction::Reload;
            }
            Action::Reload => return SkinBrowserAction::Reload,
            Action::Back if !self.query.is_empty() => {
                self.query.clear();
                return SkinBrowserAction::Reload;
            }
            Action::Back => return SkinBrowserAction::Back,
            _ => {}
        }
        SkinBrowserAction::None
    }

    fn activate(&mut self) -> SkinBrowserAction {
        let Some(entry) = self.selected() else {
            return SkinBrowserAction::None;
        };
        if entry.is_local {
            return SkinBrowserAction::Apply(entry.filename.clone());
        }
        if self.installing.is_some() {
            return SkinBrowserAction::None;
        }
        let action = SkinBrowserAction::Install {
            md5: entry.md5.clone(),
            filename: entry.filename.clone(),
        };
        self.installing = Some(entry.md5.clone());
        action
    }

    // ── paging ───────────────────────────────────────────────────────────

    /// The next online page should be fetched: the cursor is near the end
    /// of what is loaded (or every loaded skin was hidden).
    pub fn wants_more(&self) -> bool {
        self.source == Source::Online
            && self.has_more
            && !self.loading
            && self.error.is_none()
            && self.list_state.selected().unwrap_or(0) + PREFETCH_ROWS >= self.entries.len()
    }

    /// Add a fetched online page requested at `requested_offset`.
    pub fn apply_page(&mut self, page: SkinPage, requested_offset: usize) {
        self.loading = false;
        if requested_offset == 0 {
            self.entries = page.entries;
        } else {
            self.entries.extend(page.entries);
        }
        self.offset = page.next_offset;
        self.has_more = page.has_more;
        self.total = page.total;
        if self.list_state.selected().is_none() {
            self.list_state.select(Some(0));
        }
    }

    pub fn set_local(&mut self, entries: Vec<SkinEntry>) {
        self.loading = false;
        self.has_more = false;
        self.total = Some(entries.len());
        self.entries = entries;
        self.list_state.select(Some(0));
    }

    /// Skin `md5` was installed as `filename`.
    pub fn mark_installed(&mut self, md5: &str, filename: &str) {
        self.installing = None;
        for entry in &mut self.entries {
            if entry.md5 == md5 {
                entry.is_local = true;
                filename.clone_into(&mut entry.filename);
            }
        }
    }

    // ── preview ──────────────────────────────────────────────────────────

    /// Restart the preview countdown when the highlighted skin changed.
    pub fn schedule_preview(&mut self, now: Instant) {
        let Some(entry) = self.selected() else {
            self.preview = Preview::None;
            return;
        };
        if self.preview.key() == Some(entry.key()) {
            return;
        }
        self.preview = Preview::Pending {
            key: entry.key().to_owned(),
            due: now + PREVIEW_DELAY,
        };
    }

    /// Time left until a pending preview should load.
    pub fn preview_due_in(&self, now: Instant) -> Option<Duration> {
        match &self.preview {
            Preview::Pending { due, .. } => Some(due.saturating_duration_since(now)),
            _ => None,
        }
    }

    /// The preview to load now that the cursor has rested, marking it loading.
    pub fn start_due_preview(&mut self, now: Instant) -> Option<(String, PreviewSource)> {
        let Preview::Pending { key, due } = &self.preview else {
            return None;
        };
        if *due > now {
            return None;
        }
        let entry = self.selected().filter(|e| e.key() == key)?;
        let source = entry.preview_source();
        let key = key.clone();
        self.preview = Preview::Loading { key: key.clone() };
        Some((key, source))
    }

    /// Store a loaded preview; results for a skin no longer highlighted are
    /// dropped (returns `false`).
    pub fn finish_preview(&mut self, key: &str, result: Result<Box<WinampSkin>, String>) -> bool {
        if !matches!(&self.preview, Preview::Loading { key: k } if k == key) {
            return false;
        }
        let key = key.to_owned();
        self.preview = match result {
            Ok(skin) => Preview::Ready { key, skin },
            Err(error) => Preview::Failed { key, error },
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(i: usize) -> SkinEntry {
        SkinEntry {
            md5: format!("{i:032x}"),
            filename: format!("skin{i}.wsz"),
            display_name: format!("skin{i}"),
            is_local: false,
            average_color: None,
        }
    }

    fn page(range: std::ops::Range<usize>, next_offset: usize, has_more: bool) -> SkinPage {
        SkinPage {
            entries: range.map(entry).collect(),
            next_offset,
            total: Some(1000),
            has_more,
        }
    }

    fn press(b: &mut SkinBrowser, code: KeyCode) -> SkinBrowserAction {
        b.handle_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn online() -> SkinBrowser {
        let mut b = SkinBrowser::default();
        b.open(Source::Online);
        b
    }

    #[test]
    fn paging_advances_by_server_offset_and_prefetches_near_the_end() {
        let mut b = online();
        assert!(b.wants_more(), "an empty online list loads");
        b.loading = true;
        assert!(!b.wants_more(), "one request at a time");
        // 50 requested, 3 hidden as NSFW.
        b.apply_page(page(0..47, 50, true), 0);
        assert_eq!((b.entries.len(), b.offset, b.total), (47, 50, Some(1000)));
        assert!(!b.wants_more(), "cursor at the top");
        b.select(31);
        assert!(!b.wants_more());
        b.select(32);
        assert!(b.wants_more(), "within {PREFETCH_ROWS} rows of the end");

        // A page that was entirely NSFW still advances and asks again.
        b.loading = true;
        b.apply_page(page(0..0, 100, true), 50);
        assert_eq!((b.entries.len(), b.offset), (47, 100));
        assert!(b.wants_more());
        b.apply_page(page(47..90, 150, false), 100);
        assert_eq!((b.entries.len(), b.offset), (90, 150));
        b.select(usize::MAX);
        assert!(!b.wants_more(), "the end of the museum");

        b.has_more = true;
        b.error = Some("offline".into());
        assert!(!b.wants_more(), "errors stop the scroll until a reload");
        let mut local = SkinBrowser::default();
        local.open(Source::Local);
        assert!(!local.wants_more());
    }

    #[test]
    fn keys_move_install_search_and_toggle() {
        let mut b = online();
        b.apply_page(page(0..3, 3, false), 0);
        assert_eq!(press(&mut b, KeyCode::Up), SkinBrowserAction::None);
        assert_eq!(b.list_state.selected(), Some(0), "no wrap at the top");
        press(&mut b, KeyCode::End);
        press(&mut b, KeyCode::Down);
        assert_eq!(b.list_state.selected(), Some(2), "no wrap at the end");

        let install = press(&mut b, KeyCode::Enter);
        assert_eq!(
            install,
            SkinBrowserAction::Install {
                md5: entry(2).md5,
                filename: "skin2.wsz".into()
            }
        );
        assert_eq!(press(&mut b, KeyCode::Enter), SkinBrowserAction::None);
        b.mark_installed(&entry(2).md5, "skin2 (1).wsz");
        assert_eq!(
            press(&mut b, KeyCode::Enter),
            SkinBrowserAction::Apply("skin2 (1).wsz".into())
        );

        press(&mut b, KeyCode::Char('/'));
        assert_eq!(b.ctx(), Ctx::PROMPT);
        for c in "zeldaa".chars() {
            press(&mut b, KeyCode::Char(c));
        }
        press(&mut b, KeyCode::Backspace);
        assert_eq!(press(&mut b, KeyCode::Enter), SkinBrowserAction::Reload);
        assert_eq!((b.query.as_str(), b.input.is_none()), ("zelda", true));
        // Esc clears the search first, then leaves.
        assert_eq!(press(&mut b, KeyCode::Esc), SkinBrowserAction::Reload);
        assert!(b.query.is_empty());
        assert_eq!(press(&mut b, KeyCode::Esc), SkinBrowserAction::Back);

        assert_eq!(press(&mut b, KeyCode::Char('f')), SkinBrowserAction::Reload);
        assert_eq!(b.listing, SkinListing::Curated);
        assert_eq!(press(&mut b, KeyCode::Tab), SkinBrowserAction::Reload);
        assert_eq!(b.source, Source::Local);
        assert_eq!(
            press(&mut b, KeyCode::Char('f')),
            SkinBrowserAction::None,
            "online only"
        );
    }

    #[test]
    fn preview_waits_for_the_cursor_to_rest_and_drops_stale_results() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut b = online();
        b.apply_page(page(0..3, 3, false), 0);
        b.schedule_preview(t0);
        assert_eq!(b.preview_due_in(t0), Some(PREVIEW_DELAY));
        assert!(b.start_due_preview(t0 + ms(100)).is_none());

        // Moving on restarts the countdown for the new skin.
        press(&mut b, KeyCode::Down);
        b.schedule_preview(t0 + ms(200));
        assert!(b.start_due_preview(t0 + ms(300)).is_none());
        let (key, source) = b.start_due_preview(t0 + ms(450)).unwrap();
        assert_eq!(key, entry(1).md5);
        assert_eq!(source, PreviewSource::Museum(entry(1).md5));
        assert_eq!(b.preview_due_in(t0 + ms(450)), None);
        // Resting on the same skin does not reload it.
        b.schedule_preview(t0 + ms(500));
        assert!(matches!(b.preview, Preview::Loading { .. }));

        let skin = || Ok(Box::new(WinampSkin::default_skin()));
        assert!(!b.finish_preview(&entry(0).md5, skin()), "stale result");
        press(&mut b, KeyCode::Down);
        b.schedule_preview(t0 + ms(600));
        assert!(
            !b.finish_preview(&entry(1).md5, skin()),
            "the cursor moved away while it loaded"
        );
        let (key, _) = b.start_due_preview(t0 + ms(850)).unwrap();
        assert!(b.finish_preview(&key, Err("broken".into())));
        assert!(matches!(&b.preview, Preview::Failed { error, .. } if error == "broken"));

        // Installed skins preview from their file.
        let mut local = SkinBrowser::default();
        local.open(Source::Local);
        let mut e = entry(7);
        e.md5.clear();
        e.is_local = true;
        local.set_local(vec![e]);
        local.schedule_preview(t0);
        let (key, source) = local.start_due_preview(t0 + PREVIEW_DELAY).unwrap();
        assert_eq!(key, "skin7.wsz");
        assert_eq!(source, PreviewSource::Local("skin7.wsz".into()));
        assert!(local.finish_preview(&key, skin()));
        assert!(matches!(local.preview, Preview::Ready { .. }));
        local.close();
        assert!(matches!(local.preview, Preview::None));
    }
}
