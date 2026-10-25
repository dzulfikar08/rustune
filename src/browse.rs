//! Hierarchical browsing of the online providers (radio, podcasts, Subsonic,
//! Jellyfin): a pure navigation model ([`Browser`]) that asks the caller to
//! run blocking [`fetch`]es, plus the shared, thread-safe [`Services`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use crate::keymap::Action;

use crate::media::MediaItem;
use crate::providers::jellyfin::{AlbumSort, JellyfinClient};
use crate::providers::podcast::{self, Feed, PodcastClient};
use crate::providers::radio::{RadioClient, Station, StationOrder, StationQuery};
use crate::providers::subsonic::{AlbumListType, SubsonicClient};
use crate::providers::{join_dot, ProviderError, ProviderKind, ProvidersConfig, Result};

const RADIO_LIMIT: u32 = 200;
const RADIO_TOP_LIMIT: u32 = 100;
const RADIO_TAG_LIMIT: u32 = 500;
const PODCAST_LIMIT: u32 = 100;
const SUBSONIC_SEARCH_LIMIT: u32 = 50;
const SUBSONIC_ALBUM_LIMIT: u32 = 500;
const SUBSONIC_RANDOM_LIMIT: u32 = 100;
const JELLYFIN_SEARCH_LIMIT: u32 = 200;
const JELLYFIN_ALBUM_LIMIT: u32 = 500;
/// Subscription feeds downloaded at once.
const FEED_PARALLELISM: usize = 8;

/// Provider clients shared by every fetch. `Send + Sync`; network happens
/// lazily inside [`fetch`] / [`crate::report::send`] only.
pub struct Services {
    cfg: ProvidersConfig,
    radio: OnceLock<RadioClient>,
    jellyfin: Mutex<Option<JellyfinClient>>,
    jellyfin_session: Mutex<Option<(String, String)>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Services {
    pub fn new(cfg: ProvidersConfig) -> Self {
        Self {
            cfg,
            radio: OnceLock::new(),
            jellyfin: Mutex::new(None),
            jellyfin_session: Mutex::new(None),
        }
    }

    pub fn config(&self) -> &ProvidersConfig {
        &self.cfg
    }

    /// `(token, user_id)` of a Jellyfin password login made during a fetch,
    /// to persist into the config. Returned once (taken); `None` when not
    /// connected yet, already returned, or the config is token-based.
    pub fn jellyfin_session(&self) -> Option<(String, String)> {
        lock(&self.jellyfin_session).take()
    }

    /// Radio-browser client; the first call discovers a mirror (network).
    pub(crate) fn radio(&self) -> &RadioClient {
        self.radio
            .get_or_init(|| RadioClient::from_config(crate::net::agent(), &self.cfg.radio))
    }

    pub(crate) fn subsonic(&self) -> Result<SubsonicClient> {
        self.cfg
            .subsonic
            .as_ref()
            .filter(|c| c.is_configured())
            .map(SubsonicClient::new)
            .ok_or_else(|| ProviderError::Invalid("Subsonic is not configured".into()))
    }

    /// Connected Jellyfin client; logs in on first use (network).
    pub(crate) fn jellyfin(&self) -> Result<JellyfinClient> {
        let mut cached = lock(&self.jellyfin);
        if let Some(client) = cached.as_ref() {
            return Ok(client.clone());
        }
        let cfg = self
            .cfg
            .jellyfin
            .as_ref()
            .filter(|c| c.is_configured())
            .ok_or_else(|| ProviderError::Invalid("Jellyfin is not configured".into()))?;
        let client = JellyfinClient::connect(crate::net::agent(), cfg)?;
        let token_based = cfg.token.as_deref().is_some_and(|t| !t.trim().is_empty());
        if !token_based {
            *lock(&self.jellyfin_session) =
                Some((client.token().to_string(), client.user_id().to_string()));
        }
        *cached = Some(client.clone());
        Ok(client)
    }

    /// Jellyfin item ID of `item` when a client is already connected (no
    /// network).
    pub(crate) fn jellyfin_item_id(&self, item: &MediaItem) -> Option<String> {
        lock(&self.jellyfin)
            .as_ref()
            .and_then(|client| client.item_id(item))
    }
}

/// What a level loads. Opaque to the caller: pass it back to [`fetch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    RadioTop(StationOrder),
    RadioCountries,
    RadioCountry(String),
    RadioTags,
    RadioTag(String),
    RadioSearch(String),
    PodcastTop,
    PodcastSearch(String),
    PodcastSubscriptions(Vec<String>),
    PodcastFeed { feed_url: String, title: String },
    SubsonicSearch(String),
    SubsonicAlbumList(AlbumListType),
    SubsonicAlbum(String),
    SubsonicPlaylists,
    SubsonicPlaylist(String),
    SubsonicRandom,
    JellyfinSearch(String),
    JellyfinAlbums(AlbumSort),
    JellyfinAlbum(String),
    JellyfinPlaylists,
    JellyfinPlaylist(String),
}

/// Static submenus (no network).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Menu {
    SubsonicAlbums,
    JellyfinAlbums,
}

/// One row of a level.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// Opens a static submenu.
    Menu { label: &'static str, menu: Menu },
    /// Opens a fetched level (category, country, tag, album, playlist…).
    Folder {
        label: String,
        detail: Option<String>,
        request: Request,
    },
    /// Asks for a search term.
    Search,
    /// Opens the subscribed podcasts.
    Subscriptions,
    /// A show: opens its episodes; `f` toggles the subscription.
    Podcast {
        title: String,
        feed_url: String,
        detail: Option<String>,
    },
    /// Playable station, episode or song. `radio_uuid` is set for stations.
    Track {
        item: MediaItem,
        radio_uuid: Option<String>,
    },
}

impl Entry {
    pub fn label(&self) -> &str {
        match self {
            Self::Menu { label, .. } => label,
            Self::Folder { label, .. } => label.as_str(),
            Self::Search => "Search…",
            Self::Subscriptions => "Subscriptions",
            Self::Podcast { title, .. } => title.as_str(),
            Self::Track { item, .. } => item.title.as_str(),
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Folder { detail, .. } | Self::Podcast { detail, .. } => detail.as_deref(),
            Self::Track { item, .. } => item.subtitle.as_deref(),
            Self::Menu { .. } | Self::Search | Self::Subscriptions => None,
        }
    }
}

/// Blocking load of one level; run it on a blocking thread.
pub fn fetch(services: &Services, req: &Request) -> Result<Vec<Entry>> {
    let agent = crate::net::agent();
    match req {
        Request::RadioTop(order) => Ok(stations(services.radio().top(
            agent,
            *order,
            RADIO_TOP_LIMIT,
        )?)),
        Request::RadioCountries => Ok(services
            .radio()
            .countries(agent)?
            .into_iter()
            .map(|c| Entry::Folder {
                label: c.name,
                detail: Some(format!("{} stations", c.station_count)),
                request: Request::RadioCountry(c.code),
            })
            .collect()),
        Request::RadioCountry(code) => {
            let query = StationQuery {
                limit: RADIO_LIMIT,
                ..StationQuery::by_country(code)
            };
            Ok(stations(services.radio().search(agent, &query)?))
        }
        Request::RadioTags => Ok(services
            .radio()
            .tags(agent, RADIO_TAG_LIMIT)?
            .into_iter()
            .map(|t| Entry::Folder {
                detail: Some(format!("{} stations", t.station_count)),
                request: Request::RadioTag(t.name.clone()),
                label: t.name,
            })
            .collect()),
        Request::RadioTag(tag) => {
            let query = StationQuery {
                limit: RADIO_LIMIT,
                ..StationQuery::by_tag(tag)
            };
            Ok(stations(services.radio().search(agent, &query)?))
        }
        Request::RadioSearch(name) => {
            let query = StationQuery {
                limit: RADIO_LIMIT,
                ..StationQuery::by_name(name)
            };
            Ok(stations(services.radio().search(agent, &query)?))
        }
        Request::PodcastTop => Ok(podcasts(PodcastClient::default().top(
            agent,
            &services.cfg.podcast.country,
            PODCAST_LIMIT,
        )?)),
        Request::PodcastSearch(term) => Ok(podcasts(PodcastClient::default().search(
            agent,
            term,
            PODCAST_LIMIT,
        )?)),
        Request::PodcastSubscriptions(urls) => Ok(subscriptions(agent, urls)),
        Request::PodcastFeed { feed_url, .. } => {
            let feed = podcast::fetch_feed(agent, feed_url)?;
            Ok(feed
                .episodes
                .iter()
                .map(|e| Entry::Track {
                    item: e.to_media_item(&feed),
                    radio_uuid: None,
                })
                .collect())
        }
        Request::SubsonicSearch(query) => {
            let found = services
                .subsonic()?
                .search(agent, query, SUBSONIC_SEARCH_LIMIT)?;
            let mut out: Vec<Entry> = found.albums.into_iter().map(subsonic_album).collect();
            out.extend(tracks(found.songs));
            Ok(out)
        }
        Request::SubsonicAlbumList(kind) => Ok(services
            .subsonic()?
            .album_list(agent, *kind, 0, SUBSONIC_ALBUM_LIMIT)?
            .into_iter()
            .map(subsonic_album)
            .collect()),
        Request::SubsonicAlbum(id) => Ok(tracks(services.subsonic()?.album(agent, id)?.1)),
        Request::SubsonicPlaylists => Ok(services
            .subsonic()?
            .playlists(agent)?
            .into_iter()
            .map(|p| Entry::Folder {
                label: p.name,
                detail: count(p.song_count, "songs"),
                request: Request::SubsonicPlaylist(p.id),
            })
            .collect()),
        Request::SubsonicPlaylist(id) => Ok(tracks(services.subsonic()?.playlist(agent, id)?)),
        Request::SubsonicRandom => Ok(tracks(
            services
                .subsonic()?
                .random_songs(agent, SUBSONIC_RANDOM_LIMIT)?,
        )),
        Request::JellyfinSearch(query) => Ok(tracks(services.jellyfin()?.search(
            agent,
            query,
            JELLYFIN_SEARCH_LIMIT,
        )?)),
        Request::JellyfinAlbums(sort) => Ok(services
            .jellyfin()?
            .albums(agent, *sort, 0, JELLYFIN_ALBUM_LIMIT)?
            .into_iter()
            .map(|a| {
                let year = a.year.map(|y| y.to_string());
                Entry::Folder {
                    detail: join_dot([a.artist.as_deref(), year.as_deref()]),
                    label: a.name,
                    request: Request::JellyfinAlbum(a.id),
                }
            })
            .collect()),
        Request::JellyfinAlbum(id) => Ok(tracks(services.jellyfin()?.album_tracks(agent, id)?)),
        Request::JellyfinPlaylists => Ok(services
            .jellyfin()?
            .playlists(agent)?
            .into_iter()
            .map(|p| Entry::Folder {
                label: p.name,
                detail: count(p.track_count, "tracks"),
                request: Request::JellyfinPlaylist(p.id),
            })
            .collect()),
        Request::JellyfinPlaylist(id) => {
            Ok(tracks(services.jellyfin()?.playlist_tracks(agent, id)?))
        }
    }
}

fn stations(stations: Vec<Station>) -> Vec<Entry> {
    stations
        .into_iter()
        .map(|s| Entry::Track {
            item: s.to_media_item(),
            radio_uuid: (!s.uuid.is_empty()).then_some(s.uuid),
        })
        .collect()
}

fn podcasts(shows: Vec<podcast::Podcast>) -> Vec<Entry> {
    shows
        .into_iter()
        .map(|p| Entry::Podcast {
            detail: join_dot([p.author.as_deref(), p.genre.as_deref()]),
            title: p.title,
            feed_url: p.feed_url,
        })
        .collect()
}

fn tracks(items: Vec<MediaItem>) -> Vec<Entry> {
    items
        .into_iter()
        .map(|item| Entry::Track {
            item,
            radio_uuid: None,
        })
        .collect()
}

fn subsonic_album(album: crate::providers::subsonic::Album) -> Entry {
    let year = album.year.map(|y| y.to_string());
    Entry::Folder {
        detail: join_dot([album.artist.as_deref(), year.as_deref()]),
        label: album.name,
        request: Request::SubsonicAlbum(album.id),
    }
}

fn count(n: Option<u32>, unit: &str) -> Option<String> {
    n.map(|n| format!("{n} {unit}"))
}

/// Downloads every subscribed feed (a few at a time) for its title.
fn subscriptions(agent: &ureq::Agent, urls: &[String]) -> Vec<Entry> {
    let mut out = Vec::with_capacity(urls.len());
    for chunk in urls.chunks(FEED_PARALLELISM) {
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(chunk.len());
            for url in chunk {
                handles.push((url, scope.spawn(move || podcast::fetch_feed(agent, url))));
            }
            for (url, handle) in handles {
                let feed = handle.join().ok().and_then(|result| result.ok());
                out.push(subscription_entry(url, feed));
            }
        });
    }
    out
}

fn subscription_entry(url: &str, feed: Option<Feed>) -> Entry {
    match feed {
        Some(feed) => {
            let episodes = format!("{} episodes", feed.episodes.len());
            Entry::Podcast {
                detail: join_dot([feed.author.as_deref(), Some(episodes.as_str())]),
                title: if feed.title.trim().is_empty() {
                    url.to_string()
                } else {
                    feed.title
                },
                feed_url: url.to_string(),
            }
        }
        None => Entry::Podcast {
            title: url.to_string(),
            feed_url: url.to_string(),
            detail: Some("feed unavailable".into()),
        },
    }
}

/// Short user-facing message; never contains URLs or credentials.
fn error_message(err: &ProviderError) -> String {
    match err {
        ProviderError::Network(_) => "Network error: check your connection".into(),
        ProviderError::Status(code) => format!("Server returned HTTP {code}"),
        ProviderError::Parse(_) => "Unexpected response from the server".into(),
        ProviderError::Api { message, .. } => format!("Server error: {message}"),
        ProviderError::Auth(msg) => format!("Authentication failed: {msg}"),
        ProviderError::Invalid(msg) => format!("Invalid request: {msg}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LevelState {
    Loading,
    Ready,
    Error(String),
}

/// What the caller must do after a browser interaction.
#[derive(Debug, Clone, PartialEq)]
pub enum BrowseOutcome {
    None,
    /// Run [`fetch`] on a blocking thread, then call [`Browser::loaded`].
    Fetch {
        token: u64,
        request: Request,
    },
    /// Open the text prompt with this label; on submit call
    /// [`Browser::search`].
    Prompt(&'static str),
    /// Replace the queue with `items` and start at `index`.
    Play {
        items: Vec<MediaItem>,
        index: usize,
    },
    Enqueue(MediaItem),
    PlayNext(MediaItem),
    /// Add or remove `feed_url` in the podcast subscriptions and save.
    ToggleSubscription {
        feed_url: String,
        title: String,
    },
}

/// Unique across browsers so one result channel can serve all of them.
fn next_token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

struct Level {
    title: String,
    /// `None` for static menus.
    request: Option<Request>,
    state: LevelState,
    entries: Vec<Entry>,
    selected: usize,
    offset: usize,
    /// Token of the pending fetch (0 for static levels).
    token: u64,
}

impl Level {
    fn ready(title: String, entries: Vec<Entry>) -> Self {
        Self {
            title,
            request: None,
            state: LevelState::Ready,
            entries,
            selected: 0,
            offset: 0,
            token: 0,
        }
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    /// Marks the level loading with a fresh token and returns the fetch.
    fn reload(&mut self, request: Request) -> BrowseOutcome {
        self.token = next_token();
        self.state = LevelState::Loading;
        self.entries.clear();
        self.request = Some(request.clone());
        BrowseOutcome::Fetch {
            token: self.token,
            request,
        }
    }
}

fn root_entries(kind: ProviderKind) -> Vec<Entry> {
    let folder = |label: &str, request: Request| Entry::Folder {
        label: label.to_string(),
        detail: None,
        request,
    };
    match kind {
        ProviderKind::Radio => vec![
            folder("Top voted", Request::RadioTop(StationOrder::Votes)),
            folder("Top clicked", Request::RadioTop(StationOrder::Clicks)),
            folder("Trending", Request::RadioTop(StationOrder::Trend)),
            folder("By country", Request::RadioCountries),
            folder("By tag", Request::RadioTags),
            Entry::Search,
        ],
        ProviderKind::Podcast => vec![
            folder("Top charts", Request::PodcastTop),
            Entry::Search,
            Entry::Subscriptions,
        ],
        ProviderKind::Subsonic => vec![
            Entry::Search,
            Entry::Menu {
                label: "Albums",
                menu: Menu::SubsonicAlbums,
            },
            folder("Playlists", Request::SubsonicPlaylists),
            folder("Random songs", Request::SubsonicRandom),
        ],
        ProviderKind::Jellyfin => vec![
            Entry::Search,
            Entry::Menu {
                label: "Albums",
                menu: Menu::JellyfinAlbums,
            },
            folder("Playlists", Request::JellyfinPlaylists),
        ],
    }
}

fn menu_entries(menu: Menu) -> Vec<Entry> {
    match menu {
        Menu::SubsonicAlbums => AlbumListType::ALL
            .into_iter()
            .map(|kind| Entry::Folder {
                label: kind.display_name().to_string(),
                detail: None,
                request: Request::SubsonicAlbumList(kind),
            })
            .collect(),
        Menu::JellyfinAlbums => AlbumSort::ALL
            .into_iter()
            .map(|sort| Entry::Folder {
                label: sort.display_name().to_string(),
                detail: None,
                request: Request::JellyfinAlbums(sort),
            })
            .collect(),
    }
}

fn short_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Radio => "Radio",
        ProviderKind::Podcast => "Podcasts",
        ProviderKind::Subsonic => "Subsonic",
        ProviderKind::Jellyfin => "Jellyfin",
    }
}

fn prompt_label(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Radio => "Search stations",
        ProviderKind::Podcast => "Search podcasts",
        ProviderKind::Subsonic => "Search Subsonic",
        ProviderKind::Jellyfin => "Search Jellyfin",
    }
}

fn search_request(kind: ProviderKind, query: String) -> Request {
    match kind {
        ProviderKind::Radio => Request::RadioSearch(query),
        ProviderKind::Podcast => Request::PodcastSearch(query),
        ProviderKind::Subsonic => Request::SubsonicSearch(query),
        ProviderKind::Jellyfin => Request::JellyfinSearch(query),
    }
}

/// Navigation state of one provider tab: a static root menu plus a stack of
/// opened levels.
pub struct Browser {
    pub kind: ProviderKind,
    root: Level,
    stack: Vec<Level>,
    /// Last seen podcast subscriptions (for the ★ marker).
    subscriptions: Vec<String>,
    /// Rows shown at once by the last render (PageUp/PageDown step).
    viewport: usize,
}

impl Browser {
    pub fn new(kind: ProviderKind) -> Self {
        Self {
            kind,
            root: Level::ready(String::new(), root_entries(kind)),
            stack: Vec::new(),
            subscriptions: Vec::new(),
            viewport: 1,
        }
    }

    fn top(&self) -> &Level {
        self.stack.last().unwrap_or(&self.root)
    }

    fn top_mut(&mut self) -> &mut Level {
        self.stack.last_mut().unwrap_or(&mut self.root)
    }

    fn levels(&self) -> impl Iterator<Item = &Level> {
        std::iter::once(&self.root).chain(self.stack.iter())
    }

    /// Breadcrumb, e.g. `Radio › By country › Germany`.
    pub fn title(&self) -> String {
        let mut title = short_name(self.kind).to_string();
        for level in &self.stack {
            title.push_str(" › ");
            title.push_str(&level.title);
        }
        title
    }

    pub fn state(&self) -> &LevelState {
        &self.top().state
    }

    /// Rows of the current level (empty while loading or failed).
    pub fn entries(&self) -> &[Entry] {
        &self.top().entries
    }

    pub fn len(&self) -> usize {
        self.top().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.top().entries.is_empty()
    }

    pub fn selected(&self) -> usize {
        self.top().selected
    }

    /// Index of the first visible row (mouse hit-testing).
    pub fn offset(&self) -> usize {
        self.top().offset
    }

    /// Whether `entry` is a subscribed podcast.
    pub fn subscribed(&self, entry: &Entry) -> bool {
        match entry {
            Entry::Podcast { feed_url, .. } => self.subscriptions.iter().any(|s| s == feed_url),
            _ => false,
        }
    }

    /// Keeps the selection visible in `height` rows; stores the scroll
    /// offset and returns the visible index range.
    pub fn window(&mut self, height: usize) -> (usize, usize) {
        let height = height.max(1);
        self.viewport = height;
        let level = self.top_mut();
        let len = level.entries.len();
        let mut offset = level.offset.min(len.saturating_sub(height));
        if level.selected < offset {
            offset = level.selected;
        } else if level.selected >= offset + height {
            offset = level.selected + 1 - height;
        }
        level.offset = offset;
        (offset, (offset + height).min(len))
    }

    fn set_subscriptions(&mut self, subscriptions: &[String]) {
        if self.subscriptions.as_slice() != subscriptions {
            self.subscriptions = subscriptions.to_vec();
        }
    }

    fn select(&mut self, index: usize) {
        let level = self.top_mut();
        level.selected = index.min(level.entries.len().saturating_sub(1));
    }

    /// A list action from the key table or the palette.
    pub fn handle(&mut self, action: Action, subscriptions: &[String]) -> BrowseOutcome {
        self.set_subscriptions(subscriptions);
        let selected = self.selected();
        match action {
            Action::Down => self.select(selected + 1),
            Action::Up => self.select(selected.saturating_sub(1)),
            Action::Top => self.select(0),
            Action::Bottom => self.select(usize::MAX),
            Action::PageDown => self.select(selected.saturating_add(self.viewport)),
            Action::PageUp => self.select(selected.saturating_sub(self.viewport)),
            Action::Activate => return self.enter(),
            Action::Back => {
                self.stack.pop();
            }
            Action::ProviderSearch => return BrowseOutcome::Prompt(prompt_label(self.kind)),
            Action::Enqueue => {
                if let Some(Entry::Track { item, .. }) = self.top().selected_entry() {
                    return BrowseOutcome::Enqueue(item.clone());
                }
            }
            Action::PlayNext => {
                if let Some(Entry::Track { item, .. }) = self.top().selected_entry() {
                    return BrowseOutcome::PlayNext(item.clone());
                }
            }
            Action::Subscribe => return self.toggle_subscription(),
            Action::Reload => return self.reload(),
            _ => {}
        }
        BrowseOutcome::None
    }

    /// A key press in a provider tab, through the shared key table.
    #[cfg(test)]
    pub fn handle_key(
        &mut self,
        key: &crossterm::event::KeyEvent,
        subscriptions: &[String],
    ) -> BrowseOutcome {
        use crate::keymap::{lookup, Ctx};
        match lookup(Ctx::PROVIDER.or(Ctx::PODCAST), key) {
            Some(action) => self.handle(action, subscriptions),
            None => BrowseOutcome::None,
        }
    }

    /// Mouse click on row `index`: select it and open/play it.
    pub fn activate(&mut self, index: usize, subscriptions: &[String]) -> BrowseOutcome {
        self.set_subscriptions(subscriptions);
        if index >= self.len() {
            return BrowseOutcome::None;
        }
        self.select(index);
        self.enter()
    }

    /// Mouse wheel: moves the selection by one row.
    pub fn scroll(&mut self, down: bool) {
        let selected = self.selected();
        self.select(if down {
            selected + 1
        } else {
            selected.saturating_sub(1)
        });
    }

    /// The search prompt was submitted: opens the results under the root.
    pub fn search(&mut self, query: String) -> BrowseOutcome {
        let query = query.trim();
        if query.is_empty() {
            return BrowseOutcome::None;
        }
        self.stack.clear();
        let title = format!("Search “{query}”");
        let request = search_request(self.kind, query.to_string());
        self.push_fetch(title, request)
    }

    /// Result of a [`BrowseOutcome::Fetch`]; stale tokens are ignored.
    pub fn loaded(&mut self, token: u64, result: Result<Vec<Entry>>) {
        let Some(level) = std::iter::once(&mut self.root)
            .chain(self.stack.iter_mut())
            .find(|l| l.token == token && l.state == LevelState::Loading)
        else {
            return;
        };
        match result {
            Ok(entries) => {
                level.selected = level.selected.min(entries.len().saturating_sub(1));
                level.entries = entries;
                level.state = LevelState::Ready;
            }
            Err(err) => {
                level.entries.clear();
                level.state = LevelState::Error(error_message(&err));
            }
        }
    }

    /// Station UUID of a stream URL listed in any open level.
    pub fn radio_uuid(&self, url: &str) -> Option<String> {
        self.levels()
            .flat_map(|level| level.entries.iter())
            .find_map(|entry| match entry {
                Entry::Track {
                    item,
                    radio_uuid: Some(uuid),
                } if item.id == url => Some(uuid.clone()),
                _ => None,
            })
    }

    /// After a subscription change: re-fetches the open Subscriptions level.
    pub fn refresh_subscriptions(&mut self, subscriptions: &[String]) -> BrowseOutcome {
        self.set_subscriptions(subscriptions);
        let urls = self.subscriptions.clone();
        match self
            .stack
            .iter_mut()
            .find(|l| matches!(l.request, Some(Request::PodcastSubscriptions(_))))
        {
            Some(level) => level.reload(Request::PodcastSubscriptions(urls)),
            None => BrowseOutcome::None,
        }
    }

    fn push_fetch(&mut self, title: String, request: Request) -> BrowseOutcome {
        let mut level = Level::ready(title, Vec::new());
        let outcome = level.reload(request);
        self.stack.push(level);
        outcome
    }

    fn enter(&mut self) -> BrowseOutcome {
        let level = self.top();
        let Some(entry) = level.selected_entry() else {
            return BrowseOutcome::None;
        };
        let (title, request) = match entry {
            Entry::Track { .. } => {
                let mut items = Vec::new();
                let mut index = 0;
                for (i, entry) in level.entries.iter().enumerate() {
                    if let Entry::Track { item, .. } = entry {
                        if i == level.selected {
                            index = items.len();
                        }
                        items.push(item.clone());
                    }
                }
                return BrowseOutcome::Play { items, index };
            }
            Entry::Search => return BrowseOutcome::Prompt(prompt_label(self.kind)),
            Entry::Menu { label, menu } => {
                let level = Level::ready((*label).to_string(), menu_entries(*menu));
                self.stack.push(level);
                return BrowseOutcome::None;
            }
            Entry::Folder { label, request, .. } => (label.clone(), request.clone()),
            Entry::Podcast {
                title, feed_url, ..
            } => (
                title.clone(),
                Request::PodcastFeed {
                    feed_url: feed_url.clone(),
                    title: title.clone(),
                },
            ),
            Entry::Subscriptions => (
                "Subscriptions".to_string(),
                Request::PodcastSubscriptions(self.subscriptions.clone()),
            ),
        };
        self.push_fetch(title, request)
    }

    fn reload(&mut self) -> BrowseOutcome {
        let subscriptions = &self.subscriptions;
        let request = match &self.top().request {
            Some(Request::PodcastSubscriptions(_)) => {
                Request::PodcastSubscriptions(subscriptions.clone())
            }
            Some(request) => request.clone(),
            None => return BrowseOutcome::None,
        };
        self.top_mut().reload(request)
    }

    fn toggle_subscription(&self) -> BrowseOutcome {
        if self.kind != ProviderKind::Podcast {
            return BrowseOutcome::None;
        }
        let level = self.top();
        let (feed_url, title) = match (level.selected_entry(), &level.request) {
            (
                Some(Entry::Podcast {
                    feed_url, title, ..
                }),
                _,
            )
            | (_, Some(Request::PodcastFeed { feed_url, title })) => (feed_url, title),
            _ => return BrowseOutcome::None,
        };
        BrowseOutcome::ToggleSubscription {
            feed_url: feed_url.clone(),
            title: title.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::SourceKind;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(browser: &mut Browser, code: KeyCode) -> BrowseOutcome {
        browser.handle_key(&key(code), &[])
    }

    fn item(id: &str) -> MediaItem {
        MediaItem {
            id: id.to_string(),
            title: id.to_string(),
            artist: None,
            album: None,
            duration: None,
            subtitle: None,
            source: SourceKind::Url,
        }
    }

    fn track(id: &str) -> Entry {
        Entry::Track {
            item: item(id),
            radio_uuid: None,
        }
    }

    fn show(feed_url: &str, title: &str) -> Entry {
        Entry::Podcast {
            title: title.to_string(),
            feed_url: feed_url.to_string(),
            detail: None,
        }
    }

    fn fetch_token(outcome: BrowseOutcome) -> (u64, Request) {
        match outcome {
            BrowseOutcome::Fetch { token, request } => (token, request),
            other => panic!("expected Fetch, got {other:?}"),
        }
    }

    #[test]
    fn root_is_static_and_ready() {
        let browser = Browser::new(ProviderKind::Radio);
        assert_eq!(browser.title(), "Radio");
        assert_eq!(browser.state(), &LevelState::Ready);
        assert_eq!(browser.len(), 6);
        assert_eq!(browser.entries()[0].label(), "Top voted");
    }

    #[test]
    fn enter_pushes_loading_level_and_back_pops() {
        let mut browser = Browser::new(ProviderKind::Radio);
        for _ in 0..3 {
            press(&mut browser, KeyCode::Char('j'));
        }
        let (token, request) = fetch_token(press(&mut browser, KeyCode::Enter));
        assert_eq!(request, Request::RadioCountries);
        assert_eq!(browser.title(), "Radio › By country");
        assert_eq!(browser.state(), &LevelState::Loading);

        browser.loaded(
            token,
            Ok(vec![Entry::Folder {
                label: "Germany".into(),
                detail: Some("10 stations".into()),
                request: Request::RadioCountry("DE".into()),
            }]),
        );
        assert_eq!(browser.state(), &LevelState::Ready);
        let (_, request) = fetch_token(press(&mut browser, KeyCode::Enter));
        assert_eq!(request, Request::RadioCountry("DE".into()));
        assert_eq!(browser.title(), "Radio › By country › Germany");

        assert_eq!(press(&mut browser, KeyCode::Esc), BrowseOutcome::None);
        assert_eq!(browser.title(), "Radio › By country");
        assert_eq!(press(&mut browser, KeyCode::Backspace), BrowseOutcome::None);
        assert_eq!(browser.title(), "Radio");
        assert_eq!(browser.selected(), 3, "root keeps its selection");
        assert_eq!(press(&mut browser, KeyCode::Backspace), BrowseOutcome::None);
        assert_eq!(browser.title(), "Radio");
    }

    #[test]
    fn static_menu_needs_no_fetch() {
        let mut browser = Browser::new(ProviderKind::Subsonic);
        press(&mut browser, KeyCode::Down);
        assert_eq!(press(&mut browser, KeyCode::Enter), BrowseOutcome::None);
        assert_eq!(browser.title(), "Subsonic › Albums");
        assert_eq!(browser.len(), AlbumListType::ALL.len());
        assert_eq!(
            press(&mut browser, KeyCode::Char('r')),
            BrowseOutcome::None,
            "static levels have nothing to reload"
        );
        let (_, request) = fetch_token(press(&mut browser, KeyCode::Enter));
        assert_eq!(request, Request::SubsonicAlbumList(AlbumListType::ALL[0]));
        assert_eq!(press(&mut browser, KeyCode::Char('x')), BrowseOutcome::None);
    }

    #[test]
    fn stale_tokens_are_ignored() {
        let mut browser = Browser::new(ProviderKind::Radio);
        let (first, _) = fetch_token(press(&mut browser, KeyCode::Enter));
        let (second, request) = fetch_token(press(&mut browser, KeyCode::Char('r')));
        assert_ne!(first, second);
        assert_eq!(request, Request::RadioTop(StationOrder::Votes));

        browser.loaded(first, Ok(vec![track("old")]));
        assert_eq!(browser.state(), &LevelState::Loading);
        browser.loaded(second, Ok(vec![track("a"), track("b")]));
        assert_eq!(browser.state(), &LevelState::Ready);
        assert_eq!(browser.len(), 2);

        // A popped level's late result is dropped too.
        press(&mut browser, KeyCode::Esc);
        press(&mut browser, KeyCode::Down);
        let (popped, _) = fetch_token(press(&mut browser, KeyCode::Enter));
        press(&mut browser, KeyCode::Esc);
        browser.loaded(popped, Ok(vec![track("x")]));
        assert_eq!(browser.title(), "Radio");
        assert_eq!(browser.len(), 6);
    }

    #[test]
    fn errors_are_short_and_reloadable() {
        let mut browser = Browser::new(ProviderKind::Radio);
        let (token, _) = fetch_token(press(&mut browser, KeyCode::Enter));
        browser.loaded(
            token,
            Err(ProviderError::Network(
                "dns failed for https://secret".into(),
            )),
        );
        match browser.state() {
            LevelState::Error(msg) => assert!(!msg.contains("secret"), "{msg}"),
            other => panic!("expected error, got {other:?}"),
        }
        assert_eq!(browser.len(), 0);
        let (token, _) = fetch_token(press(&mut browser, KeyCode::Char('r')));
        browser.loaded(token, Ok(vec![track("a")]));
        assert_eq!(browser.state(), &LevelState::Ready);
    }

    #[test]
    fn prompt_then_search_fetches_from_root() {
        let mut browser = Browser::new(ProviderKind::Radio);
        press(&mut browser, KeyCode::Enter);
        assert_eq!(
            press(&mut browser, KeyCode::Char('/')),
            BrowseOutcome::Prompt("Search stations")
        );
        assert_eq!(browser.search("   ".into()), BrowseOutcome::None);
        let (_, request) = fetch_token(browser.search(" jazz ".into()));
        assert_eq!(request, Request::RadioSearch("jazz".into()));
        assert_eq!(browser.title(), "Radio › Search “jazz”");

        let mut jelly = Browser::new(ProviderKind::Jellyfin);
        assert_eq!(
            press(&mut jelly, KeyCode::Enter),
            BrowseOutcome::Prompt("Search Jellyfin")
        );
        let (_, request) = fetch_token(jelly.search("x".into()));
        assert_eq!(request, Request::JellyfinSearch("x".into()));
    }

    #[test]
    fn play_queues_every_playable_entry() {
        let mut browser = Browser::new(ProviderKind::Subsonic);
        let (token, _) = fetch_token(browser.search("q".into()));
        let album = Entry::Folder {
            label: "Album".into(),
            detail: None,
            request: Request::SubsonicAlbum("1".into()),
        };
        browser.loaded(token, Ok(vec![album, track("a"), track("b")]));
        press(&mut browser, KeyCode::Char('G'));
        assert_eq!(
            press(&mut browser, KeyCode::Enter),
            BrowseOutcome::Play {
                items: vec![item("a"), item("b")],
                index: 1,
            }
        );
        assert_eq!(
            press(&mut browser, KeyCode::Char('a')),
            BrowseOutcome::Enqueue(item("b"))
        );
        assert_eq!(
            press(&mut browser, KeyCode::Char('A')),
            BrowseOutcome::PlayNext(item("b"))
        );
        press(&mut browser, KeyCode::Home);
        assert_eq!(press(&mut browser, KeyCode::Char('a')), BrowseOutcome::None);
        assert_eq!(
            browser.activate(1, &[]),
            BrowseOutcome::Play {
                items: vec![item("a"), item("b")],
                index: 0,
            }
        );
        assert_eq!(browser.activate(9, &[]), BrowseOutcome::None);
    }

    #[test]
    fn f_toggles_podcast_and_episode_level() {
        let subs = vec!["https://a/feed".to_string()];
        let mut browser = Browser::new(ProviderKind::Podcast);
        let (token, request) = fetch_token(browser.handle_key(&key(KeyCode::Enter), &subs));
        assert_eq!(request, Request::PodcastTop);
        browser.loaded(
            token,
            Ok(vec![
                show("https://a/feed", "A"),
                show("https://b/feed", "B"),
            ]),
        );
        assert!(browser.subscribed(&browser.entries()[0]));
        assert!(!browser.subscribed(&browser.entries()[1]));

        browser.handle_key(&key(KeyCode::Down), &subs);
        assert_eq!(
            browser.handle_key(&key(KeyCode::Char('f')), &subs),
            BrowseOutcome::ToggleSubscription {
                feed_url: "https://b/feed".into(),
                title: "B".into(),
            }
        );
        let (token, request) = fetch_token(browser.handle_key(&key(KeyCode::Enter), &subs));
        assert_eq!(
            request,
            Request::PodcastFeed {
                feed_url: "https://b/feed".into(),
                title: "B".into(),
            }
        );
        assert_eq!(browser.title(), "Podcasts › Top charts › B");
        browser.loaded(token, Ok(vec![track("ep1")]));
        assert_eq!(
            browser.handle_key(&key(KeyCode::Char('f')), &subs),
            BrowseOutcome::ToggleSubscription {
                feed_url: "https://b/feed".into(),
                title: "B".into(),
            }
        );

        let mut radio = Browser::new(ProviderKind::Radio);
        assert_eq!(press(&mut radio, KeyCode::Char('f')), BrowseOutcome::None);
    }

    #[test]
    fn subscriptions_level_refreshes() {
        let mut browser = Browser::new(ProviderKind::Podcast);
        let one = vec!["https://a/feed".to_string()];
        assert_eq!(browser.refresh_subscriptions(&one), BrowseOutcome::None);
        browser.handle_key(&key(KeyCode::End), &one);
        let (token, request) = fetch_token(browser.handle_key(&key(KeyCode::Enter), &one));
        assert_eq!(request, Request::PodcastSubscriptions(one.clone()));
        browser.loaded(token, Ok(vec![show("https://a/feed", "A")]));

        let two = vec!["https://a/feed".to_string(), "https://b/feed".to_string()];
        let (token, request) = fetch_token(browser.refresh_subscriptions(&two));
        assert_eq!(request, Request::PodcastSubscriptions(two.clone()));
        assert_eq!(browser.state(), &LevelState::Loading);
        browser.loaded(token, Ok(vec![show("https://a/feed", "A")]));
        assert_eq!(browser.state(), &LevelState::Ready);
    }

    #[test]
    fn radio_uuid_found_in_open_levels() {
        let mut browser = Browser::new(ProviderKind::Radio);
        let (token, _) = fetch_token(press(&mut browser, KeyCode::Enter));
        browser.loaded(
            token,
            Ok(vec![Entry::Track {
                item: item("http://stream"),
                radio_uuid: Some("abc-1".into()),
            }]),
        );
        assert_eq!(browser.radio_uuid("http://stream"), Some("abc-1".into()));
        assert_eq!(browser.radio_uuid("http://other"), None);
    }

    #[test]
    fn window_and_paging_follow_selection() {
        let mut browser = Browser::new(ProviderKind::Radio);
        let (token, _) = fetch_token(press(&mut browser, KeyCode::Enter));
        let entries = (0..50).map(|i| track(&i.to_string())).collect();
        browser.loaded(token, Ok(entries));
        assert_eq!(browser.window(10), (0, 10));
        press(&mut browser, KeyCode::PageDown);
        assert_eq!(browser.selected(), 10);
        assert_eq!(browser.window(10), (1, 11));
        assert_eq!(browser.offset(), 1);
        press(&mut browser, KeyCode::End);
        assert_eq!(browser.window(10), (40, 50));
        press(&mut browser, KeyCode::PageUp);
        assert_eq!(browser.selected(), 39);
        browser.scroll(false);
        assert_eq!(browser.selected(), 38);
        browser.scroll(true);
        browser.scroll(true);
        assert_eq!(browser.selected(), 40);
        assert_eq!(browser.activate(41, &[]), {
            let items: Vec<MediaItem> = (0..50).map(|i| item(&i.to_string())).collect();
            BrowseOutcome::Play { items, index: 41 }
        });
    }

    #[test]
    fn subscription_entry_falls_back_to_url() {
        assert_eq!(
            subscription_entry("https://a/feed", None).label(),
            "https://a/feed"
        );
        let feed = Feed {
            title: "Show".into(),
            ..Feed::default()
        };
        let entry = subscription_entry("https://a/feed", Some(feed));
        assert_eq!(entry.label(), "Show");
        assert_eq!(entry.detail(), Some("0 episodes"));
    }

    #[test]
    fn services_without_network() {
        let services = Services::new(ProvidersConfig::default());
        assert_eq!(services.jellyfin_session(), None);
        assert!(services.subsonic().is_err());
        assert!(services.jellyfin().is_err());
        assert_eq!(services.jellyfin_item_id(&item("http://x")), None);
    }
}
