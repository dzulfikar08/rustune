//! Play queue: ordered items, the playing track, a display cursor, shuffle
//! and repeat.
//!
//! `items` is the playlist in display order. `order` is the play order (a
//! permutation of item indices; identity unless shuffled). `next_pos` is the
//! position in `order` of the track that plays after the current one; when a
//! track is current it sits at `order[next_pos - 1]`.

use crate::media::MediaItem;
use crate::view::RepeatMode;

/// Tiny SplitMix64 PRNG; plenty for shuffling and avoids a `rand` dependency.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Seed from the wall clock and process id.
    pub fn from_time() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self::new(nanos ^ (u64::from(std::process::id()) << 32))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform-ish integer in `0..n` (multiply-shift; `n` must be > 0).
    pub fn below(&mut self, n: usize) -> usize {
        ((u128::from(self.next_u64()) * n as u128) >> 64) as usize
    }

    /// Fisher–Yates shuffle.
    pub fn shuffle<T>(&mut self, slice: &mut [T]) {
        for i in (1..slice.len()).rev() {
            let j = self.below(i + 1);
            slice.swap(i, j);
        }
    }
}

#[derive(Debug, Clone)]
pub struct Queue {
    items: Vec<MediaItem>,
    order: Vec<usize>,
    next_pos: usize,
    current: Option<usize>,
    cursor: usize,
    shuffle: bool,
    repeat: RepeatMode,
    rng: Rng,
}

impl Default for Queue {
    fn default() -> Self {
        Self::with_rng(Rng::from_time())
    }
}

impl Queue {
    pub fn with_rng(rng: Rng) -> Self {
        Self {
            items: Vec::new(),
            order: Vec::new(),
            next_pos: 0,
            current: None,
            cursor: 0,
            shuffle: false,
            repeat: RepeatMode::Off,
            rng,
        }
    }

    pub fn items(&self) -> &[MediaItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Index into `items()` of the current track.
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    pub fn current(&self) -> Option<&MediaItem> {
        self.current.map(|i| &self.items[i])
    }

    /// The current track, for updating what was learnt while resolving it.
    pub fn current_mut(&mut self) -> Option<&mut MediaItem> {
        self.current.map(|i| &mut self.items[i])
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_cursor(&mut self, index: usize) {
        self.cursor = index.min(self.items.len().saturating_sub(1));
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    pub fn set_repeat(&mut self, repeat: RepeatMode) {
        self.repeat = repeat;
    }

    pub fn cycle_repeat(&mut self) -> RepeatMode {
        self.repeat = self.repeat.cycle();
        self.repeat
    }

    pub fn toggle_shuffle(&mut self) -> bool {
        self.set_shuffle(!self.shuffle);
        self.shuffle
    }

    /// Enabling shuffle keeps the current track first in the new order;
    /// disabling restores display order, continuing after the current track.
    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        if on {
            self.reshuffle(self.current);
            self.next_pos = usize::from(self.current.is_some());
        } else {
            // What was going to play next keeps playing next.
            let upcoming = self.order.get(self.next_pos).copied();
            self.order = (0..self.items.len()).collect();
            self.next_pos = match (self.current, upcoming) {
                (Some(c), _) => c + 1,
                (None, Some(u)) => u,
                (None, None) => self.items.len(),
            };
        }
    }

    /// Build a fresh shuffled order, with `first` (if any) at the front.
    fn reshuffle(&mut self, first: Option<usize>) {
        self.order = (0..self.items.len())
            .filter(|&i| Some(i) != first)
            .collect();
        self.rng.shuffle(&mut self.order);
        if let Some(f) = first {
            self.order.insert(0, f);
        }
    }

    /// Replace the whole queue. `start` (an index into `items`) becomes the
    /// current track; the cursor follows it.
    pub fn replace(&mut self, items: Vec<MediaItem>, start: Option<usize>) {
        self.items = items;
        let start = start.filter(|&s| s < self.items.len());
        if self.shuffle {
            self.reshuffle(start);
        } else {
            self.order = (0..self.items.len()).collect();
        }
        self.current = None;
        self.next_pos = 0;
        self.cursor = 0;
        if let Some(s) = start {
            self.start_at_item(s);
            self.cursor = s;
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.next_pos = 0;
        self.current = None;
        self.cursor = 0;
    }

    /// Append to the end of the playlist. While shuffled, the new track is
    /// slotted at a random point among the not-yet-played tracks.
    pub fn enqueue(&mut self, item: MediaItem) -> usize {
        let idx = self.items.len();
        self.items.push(item);
        if self.shuffle {
            let span = self.order.len() - self.next_pos + 1;
            let at = self.next_pos + self.rng.below(span);
            self.order.insert(at, idx);
        } else {
            self.order.push(idx);
        }
        idx
    }

    /// Insert so the item plays right after the current track. Returns its
    /// index in `items`.
    pub fn play_next(&mut self, item: MediaItem) -> usize {
        let k = match self.current {
            Some(c) => c + 1,
            None if !self.shuffle => self.next_pos.min(self.items.len()),
            None => self.items.len(),
        };
        self.items.insert(k, item);
        for o in &mut self.order {
            if *o >= k {
                *o += 1;
            }
        }
        if let Some(c) = &mut self.current {
            if *c >= k {
                *c += 1;
            }
        }
        if self.cursor >= k && self.items.len() > 1 {
            self.cursor += 1;
        }
        self.order.insert(self.next_pos, k);
        k
    }

    /// Remove an item. Removing the current track leaves nothing current, but
    /// `next_track()` still continues with the track that followed it.
    pub fn remove(&mut self, index: usize) -> Option<MediaItem> {
        if index >= self.items.len() {
            return None;
        }
        let item = self.items.remove(index);
        if let Some(p) = self.order.iter().position(|&o| o == index) {
            self.order.remove(p);
            if p < self.next_pos {
                self.next_pos -= 1;
            }
        }
        for o in &mut self.order {
            if *o > index {
                *o -= 1;
            }
        }
        self.current = match self.current {
            Some(c) if c == index => None,
            Some(c) if c > index => Some(c - 1),
            other => other,
        };
        if self.cursor > index || self.cursor >= self.items.len() {
            self.cursor = self.cursor.saturating_sub(1);
        }
        Some(item)
    }

    /// Move an item in display order. Unshuffled play order follows display
    /// order, so the upcoming track is whatever now follows the current one.
    pub fn move_item(&mut self, from: usize, to: usize) {
        let len = self.items.len();
        if from >= len || to >= len || from == to {
            return;
        }
        let map = |x: usize| -> usize {
            if x == from {
                to
            } else if from < to && (from + 1..=to).contains(&x) {
                x - 1
            } else if from > to && (to..from).contains(&x) {
                x + 1
            } else {
                x
            }
        };
        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.current = self.current.map(map);
        self.cursor = map(self.cursor);
        if self.shuffle {
            for o in &mut self.order {
                *o = map(*o);
            }
        } else {
            let upcoming = self.order.get(self.next_pos).copied().map(map);
            self.order = (0..len).collect();
            self.next_pos = match (self.current, upcoming) {
                (Some(c), _) => c + 1,
                (None, Some(u)) => u,
                (None, None) => len,
            };
        }
    }

    /// Make `items[index]` current (Winamp double-click).
    pub fn jump(&mut self, index: usize) -> Option<&MediaItem> {
        if index >= self.items.len() {
            return None;
        }
        self.start_at_item(index);
        self.current()
    }

    fn start_at_item(&mut self, index: usize) {
        if let Some(p) = self.order.iter().position(|&o| o == index) {
            self.start_at_pos(p);
        }
    }

    fn start_at_pos(&mut self, p: usize) {
        self.current = Some(self.order[p]);
        self.next_pos = p + 1;
    }

    /// Advance on user request ("next" button): repeat-one does not pin the
    /// track; repeat-all wraps (reshuffling when shuffled).
    pub fn next_track(&mut self) -> Option<&MediaItem> {
        if self.next_pos < self.order.len() {
            self.start_at_pos(self.next_pos);
            return self.current();
        }
        if self.repeat == RepeatMode::All && !self.items.is_empty() {
            if self.shuffle {
                self.reshuffle(None);
                // Avoid replaying the track that just ended straight away.
                if self.order.len() > 1 && Some(self.order[0]) == self.current {
                    let j = 1 + self.rng.below(self.order.len() - 1);
                    self.order.swap(0, j);
                }
            }
            self.start_at_pos(0);
            return self.current();
        }
        None
    }

    /// Advance because the current track ended: repeat-one replays it.
    pub fn advance_auto(&mut self) -> Option<&MediaItem> {
        if self.repeat == RepeatMode::One && self.current.is_some() {
            return self.current();
        }
        self.next_track()
    }

    /// What `advance_auto` would return, without changing state (for gapless
    /// preloading). `None` when the queue ends, or when a shuffled repeat-all
    /// wrap is pending (the next cycle's order is not drawn yet).
    pub fn peek_next(&self) -> Option<&MediaItem> {
        if self.repeat == RepeatMode::One {
            if let Some(c) = self.current {
                return Some(&self.items[c]);
            }
        }
        if let Some(&i) = self.order.get(self.next_pos) {
            return Some(&self.items[i]);
        }
        if self.repeat == RepeatMode::All && !self.shuffle {
            return self.order.first().map(|&i| &self.items[i]);
        }
        None
    }

    /// Step back in play order; wraps only with repeat-all.
    pub fn prev_track(&mut self) -> Option<&MediaItem> {
        let back = if self.current.is_some() { 2 } else { 1 };
        if self.next_pos >= back {
            self.start_at_pos(self.next_pos - back);
            return self.current();
        }
        if self.repeat == RepeatMode::All && !self.order.is_empty() {
            self.start_at_pos(self.order.len() - 1);
            return self.current();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::SourceKind;

    fn item(n: usize) -> MediaItem {
        MediaItem {
            id: format!("/m/{n}.mp3"),
            title: format!("t{n}"),
            artist: None,
            album: None,
            duration: None,
            subtitle: None,
            source: SourceKind::Local,
        }
    }

    fn queue(n: usize) -> Queue {
        let mut q = Queue::with_rng(Rng::new(42));
        q.replace((0..n).map(item).collect(), None);
        q
    }

    fn title(q: Option<&MediaItem>) -> Option<String> {
        q.map(|i| i.title.clone())
    }

    fn assert_permutation(q: &Queue) {
        let mut o = q.order.clone();
        o.sort_unstable();
        assert_eq!(o, (0..q.len()).collect::<Vec<_>>());
        if let Some(c) = q.current {
            assert!(q.next_pos >= 1);
            assert_eq!(q.order[q.next_pos - 1], c);
        }
    }

    #[test]
    fn rng_is_deterministic_and_in_range() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
            assert_eq!(a.below(3), b.below(3));
            assert!(a.below(3) < 3 && b.below(3) < 3);
        }
        let mut v: Vec<u32> = (0..50).collect();
        a.shuffle(&mut v);
        let mut s = v.clone();
        s.sort_unstable();
        assert_eq!(s, (0..50).collect::<Vec<_>>());
        assert_ne!(v, s, "50 elements should not shuffle to identity");
    }

    #[test]
    fn empty_queue_is_inert() {
        let mut q = queue(0);
        assert!(q.next_track().is_none());
        assert!(q.prev_track().is_none());
        assert!(q.advance_auto().is_none());
        assert!(q.peek_next().is_none());
        assert!(q.remove(0).is_none());
        q.set_cursor(5);
        assert_eq!(q.cursor(), 0);
        q.set_repeat(RepeatMode::All);
        assert!(q.next_track().is_none());
        q.set_shuffle(true);
        assert!(q.next_track().is_none());
    }

    #[test]
    fn linear_next_prev_and_end() {
        let mut q = queue(3);
        assert_eq!(title(q.next_track()), Some("t0".into()));
        assert_eq!(title(q.next_track()), Some("t1".into()));
        assert_eq!(title(q.next_track()), Some("t2".into()));
        assert!(q.next_track().is_none(), "repeat off stops at the end");
        assert_eq!(
            q.current_index(),
            Some(2),
            "current stays on the last track"
        );
        assert_eq!(title(q.prev_track()), Some("t1".into()));
        assert_eq!(title(q.prev_track()), Some("t0".into()));
        assert!(q.prev_track().is_none());
        assert_eq!(q.current_index(), Some(0));
    }

    #[test]
    fn repeat_all_wraps_both_ways() {
        let mut q = queue(3);
        q.set_repeat(RepeatMode::All);
        q.jump(2);
        assert_eq!(title(q.peek_next()), Some("t0".into()));
        assert_eq!(title(q.next_track()), Some("t0".into()));
        assert_eq!(title(q.prev_track()), Some("t2".into()));
    }

    #[test]
    fn repeat_one_pins_auto_advance_but_not_manual_next() {
        let mut q = queue(3);
        q.set_repeat(RepeatMode::One);
        q.jump(1);
        assert_eq!(title(q.peek_next()), Some("t1".into()));
        assert_eq!(title(q.advance_auto()), Some("t1".into()));
        assert_eq!(title(q.next_track()), Some("t2".into()));
        assert!(q.next_track().is_none());
    }

    #[test]
    fn repeat_cycle_order() {
        let mut q = queue(1);
        assert_eq!(q.cycle_repeat(), RepeatMode::All);
        assert_eq!(q.cycle_repeat(), RepeatMode::One);
        assert_eq!(q.cycle_repeat(), RepeatMode::Off);
    }

    #[test]
    fn shuffle_keeps_current_first_and_visits_all() {
        let mut q = queue(20);
        q.jump(7);
        q.set_shuffle(true);
        assert_permutation(&q);
        assert_eq!(q.order[0], 7);
        assert_eq!(q.current_index(), Some(7));
        let mut seen: Vec<usize> = vec![7];
        while let Some(i) = q.next_track() {
            seen.push(i.title[1..].parse().unwrap());
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn unshuffle_continues_after_current_in_display_order() {
        let mut q = queue(10);
        q.jump(4);
        q.set_shuffle(true);
        q.next_track();
        let cur = q.current_index().unwrap();
        q.set_shuffle(false);
        assert_permutation(&q);
        assert_eq!(q.current_index(), Some(cur));
        if cur + 1 < 10 {
            assert_eq!(q.next_track().map(|i| i.id.clone()), Some(item(cur + 1).id));
        }
    }

    #[test]
    fn shuffled_repeat_all_reshuffles_without_immediate_repeat() {
        let mut q = queue(5);
        q.set_repeat(RepeatMode::All);
        q.set_shuffle(true);
        let first = q.next_track().map(|i| i.id.clone());
        assert!(first.is_some());
        for _ in 0..4 {
            q.next_track();
        }
        let last = q.current_index();
        assert!(
            q.peek_next().is_none(),
            "wrap with shuffle is not predictable"
        );
        q.next_track();
        assert_ne!(q.current_index(), last);
        assert_permutation(&q);
    }

    #[test]
    fn single_track_repeat_all_replays() {
        let mut q = queue(1);
        q.set_repeat(RepeatMode::All);
        q.set_shuffle(true);
        assert_eq!(title(q.next_track()), Some("t0".into()));
        assert_eq!(title(q.next_track()), Some("t0".into()));
    }

    #[test]
    fn replace_with_start_sets_current_and_cursor() {
        let mut q = queue(0);
        q.replace((0..4).map(item).collect(), Some(2));
        assert_eq!(q.current_index(), Some(2));
        assert_eq!(q.cursor(), 2);
        assert_eq!(title(q.next_track()), Some("t3".into()));
        q.replace((0..4).map(item).collect(), Some(99));
        assert_eq!(q.current_index(), None);
        assert_eq!(title(q.next_track()), Some("t0".into()));
    }

    #[test]
    fn replace_while_shuffled_puts_start_first() {
        let mut q = queue(0);
        q.set_shuffle(true);
        q.replace((0..8).map(item).collect(), Some(5));
        assert_eq!(q.order[0], 5);
        assert_permutation(&q);
    }

    #[test]
    fn remove_current_continues_with_following_track() {
        let mut q = queue(4);
        q.jump(1);
        let removed = q.remove(1).unwrap();
        assert_eq!(removed.title, "t1");
        assert_eq!(q.current_index(), None);
        assert_permutation(&q);
        assert_eq!(title(q.next_track()), Some("t2".into()));
    }

    #[test]
    fn remove_current_then_prev_goes_to_previous() {
        let mut q = queue(4);
        q.jump(2);
        q.remove(2);
        assert_eq!(title(q.prev_track()), Some("t1".into()));
    }

    #[test]
    fn remove_before_and_after_current_shifts_indices() {
        let mut q = queue(5);
        q.jump(2);
        q.remove(0);
        assert_eq!(q.current_index(), Some(1));
        assert_eq!(q.current().unwrap().title, "t2");
        q.remove(3);
        assert_eq!(q.current().unwrap().title, "t2");
        assert_permutation(&q);
        assert_eq!(title(q.next_track()), Some("t3".into()));
        assert!(q.next_track().is_none());
    }

    #[test]
    fn remove_last_current_ends_queue() {
        let mut q = queue(2);
        q.jump(1);
        q.remove(1);
        assert!(q.next_track().is_none());
        q.set_repeat(RepeatMode::All);
        assert_eq!(title(q.next_track()), Some("t0".into()));
    }

    #[test]
    fn remove_clamps_cursor() {
        let mut q = queue(3);
        q.set_cursor(2);
        q.remove(2);
        assert_eq!(q.cursor(), 1);
        q.set_cursor(0);
        q.remove(1);
        assert_eq!(q.cursor(), 0);
        q.remove(0);
        assert_eq!(q.cursor(), 0);
        assert!(q.is_empty());
    }

    #[test]
    fn enqueue_appends_and_is_reachable() {
        let mut q = queue(2);
        q.jump(1);
        assert!(q.peek_next().is_none());
        let idx = q.enqueue(item(9));
        assert_eq!(idx, 2);
        assert_eq!(title(q.peek_next()), Some("t9".into()));
        assert_eq!(title(q.next_track()), Some("t9".into()));
    }

    #[test]
    fn enqueue_while_shuffled_lands_in_unplayed_part() {
        let mut q = queue(6);
        q.set_shuffle(true);
        q.next_track();
        q.next_track();
        let played: Vec<usize> = q.order[..q.next_pos].to_vec();
        let idx = q.enqueue(item(99));
        assert_permutation(&q);
        let pos = q.order.iter().position(|&o| o == idx).unwrap();
        assert!(pos >= q.next_pos);
        assert_eq!(&q.order[..q.next_pos], &played[..]);
    }

    #[test]
    fn play_next_inserts_after_current() {
        let mut q = queue(3);
        q.jump(0);
        let k = q.play_next(item(7));
        assert_eq!(k, 1);
        assert_eq!(q.items()[1].title, "t7");
        assert_permutation(&q);
        assert_eq!(title(q.next_track()), Some("t7".into()));
        assert_eq!(title(q.next_track()), Some("t1".into()));
    }

    #[test]
    fn play_next_while_shuffled_and_without_current() {
        let mut q = queue(5);
        q.set_shuffle(true);
        q.next_track();
        q.play_next(item(8));
        assert_permutation(&q);
        assert_eq!(title(q.next_track()), Some("t8".into()));

        let mut q = queue(3);
        q.play_next(item(5));
        assert_permutation(&q);
        assert_eq!(title(q.next_track()), Some("t5".into()));
    }

    #[test]
    fn move_item_keeps_current_and_order_consistent() {
        let mut q = queue(5);
        q.jump(1);
        q.move_item(1, 3);
        assert_eq!(q.current_index(), Some(3));
        assert_eq!(q.current().unwrap().title, "t1");
        assert_permutation(&q);
        assert_eq!(title(q.next_track()), Some("t4".into()));

        let mut q = queue(5);
        q.jump(2);
        q.move_item(4, 0);
        assert_eq!(q.current().unwrap().title, "t2");
        assert_eq!(q.items()[0].title, "t4");
        assert_permutation(&q);
        assert_eq!(title(q.next_track()), Some("t3".into()));

        // Out of range and no-op moves are ignored.
        q.move_item(9, 0);
        q.move_item(1, 1);
        assert_permutation(&q);
    }

    #[test]
    fn move_item_while_shuffled_preserves_play_order() {
        let mut q = queue(6);
        q.set_shuffle(true);
        q.next_track();
        let upcoming: Vec<String> = q.order[q.next_pos..]
            .iter()
            .map(|&i| q.items[i].id.clone())
            .collect();
        q.move_item(0, 5);
        assert_permutation(&q);
        let after: Vec<String> = q.order[q.next_pos..]
            .iter()
            .map(|&i| q.items[i].id.clone())
            .collect();
        assert_eq!(upcoming, after);
    }

    #[test]
    fn clear_resets_everything() {
        let mut q = queue(3);
        q.jump(1);
        q.set_cursor(2);
        q.clear();
        assert!(q.is_empty());
        assert_eq!(q.current_index(), None);
        assert_eq!(q.cursor(), 0);
        assert!(q.next_track().is_none());
    }

    #[test]
    fn jump_out_of_range_is_ignored() {
        let mut q = queue(2);
        assert!(q.jump(2).is_none());
        assert_eq!(q.current_index(), None);
    }
}
