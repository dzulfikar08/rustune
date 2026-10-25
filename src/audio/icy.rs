//! Shoutcast/Icecast in-band metadata ("ICY"): strips the metadata blocks that
//! servers interleave every `icy-metaint` bytes and reports `StreamTitle`
//! changes. Handles blocks split across arbitrary read boundaries.

use std::io::{self, Read};

enum State {
    /// Audio bytes left before the next metadata length byte.
    Audio(usize),
    /// Expecting the length byte.
    Len,
    /// Collecting `left` more metadata bytes into `meta`.
    Meta { left: usize },
}

pub struct IcyReader<R> {
    inner: R,
    metaint: usize,
    state: State,
    meta: Vec<u8>,
    last_title: String,
    on_title: Box<dyn FnMut(&str) + Send>,
}

impl<R: Read> IcyReader<R> {
    pub fn new(inner: R, metaint: usize, on_title: Box<dyn FnMut(&str) + Send>) -> Self {
        let metaint = metaint.max(1);
        Self {
            inner,
            metaint,
            state: State::Audio(metaint),
            meta: Vec::with_capacity(255 * 16),
            last_title: String::new(),
            on_title,
        }
    }

    fn finish_meta(&mut self) {
        if let Some(title) = parse_stream_title(&self.meta) {
            if !title.is_empty() && title != self.last_title {
                (self.on_title)(&title);
                self.last_title = title;
            }
        }
        self.meta.clear();
    }
}

impl<R: Read> Read for IcyReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.state {
                State::Audio(0) => self.state = State::Len,
                State::Audio(left) => {
                    let n = buf.len().min(left);
                    let got = self.inner.read(&mut buf[..n])?;
                    if got > 0 {
                        self.state = State::Audio(left - got);
                    }
                    return Ok(got);
                }
                State::Len => {
                    let mut b = [0u8; 1];
                    if self.inner.read(&mut b)? == 0 {
                        return Ok(0);
                    }
                    let len = usize::from(b[0]) * 16;
                    self.state = if len == 0 {
                        State::Audio(self.metaint)
                    } else {
                        self.meta.clear();
                        State::Meta { left: len }
                    };
                }
                State::Meta { left } => {
                    let start = self.meta.len();
                    self.meta.resize(start + left, 0);
                    let got = self.inner.read(&mut self.meta[start..]);
                    let got = match got {
                        Ok(n) => n,
                        Err(e) => {
                            self.meta.truncate(start);
                            return Err(e);
                        }
                    };
                    self.meta.truncate(start + got);
                    if got == 0 {
                        return Ok(0);
                    }
                    if got == left {
                        self.finish_meta();
                        self.state = State::Audio(self.metaint);
                    } else {
                        self.state = State::Meta { left: left - got };
                    }
                }
            }
        }
    }
}

/// Extract `StreamTitle` from a metadata block such as
/// `StreamTitle='Artist - Title';StreamUrl='';` padded with NULs. Titles may
/// contain apostrophes; the value ends at the first `';` (or the last `'`).
pub fn parse_stream_title(block: &[u8]) -> Option<String> {
    let end = block.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let text = decode_text(&block[..end]);
    let start = text.find("StreamTitle='")? + "StreamTitle='".len();
    let rest = &text[start..];
    let stop = rest
        .find("';")
        .or_else(|| rest.rfind('\''))
        .unwrap_or(rest.len());
    Some(rest[..stop].trim().to_owned())
}

/// UTF-8 when valid, otherwise Latin-1 (common on older servers).
fn decode_text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_owned(),
        Err(_) => b.iter().map(|&c| char::from(c)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Reader returning at most `step` bytes per call, cycling through sizes.
    struct Trickle {
        data: Vec<u8>,
        pos: usize,
        steps: Vec<usize>,
        i: usize,
    }

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let step = self.steps[self.i % self.steps.len()];
            self.i += 1;
            let n = buf.len().min(step).min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    fn meta_block(text: &str) -> Vec<u8> {
        let blocks = text.len().div_ceil(16);
        let mut v = vec![blocks as u8];
        v.extend_from_slice(text.as_bytes());
        v.resize(1 + blocks * 16, 0);
        v
    }

    fn build_stream(metaint: usize, chunks: usize, titles: &[&str]) -> (Vec<u8>, Vec<u8>) {
        let mut wire = Vec::new();
        let mut audio = Vec::new();
        for c in 0..chunks {
            let a: Vec<u8> = (0..metaint).map(|i| (c * 31 + i) as u8).collect();
            wire.extend_from_slice(&a);
            audio.extend_from_slice(&a);
            match titles.get(c) {
                Some(t) if !t.is_empty() => {
                    wire.extend(meta_block(&format!("StreamTitle='{t}';StreamUrl='';")))
                }
                _ => wire.push(0),
            }
        }
        (wire, audio)
    }

    fn run(
        wire: Vec<u8>,
        metaint: usize,
        steps: Vec<usize>,
        out_chunk: usize,
    ) -> (Vec<u8>, Vec<String>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let mut r = IcyReader::new(
            Trickle {
                data: wire,
                pos: 0,
                steps,
                i: 0,
            },
            metaint,
            Box::new(move |t: &str| sink.lock().unwrap().push(t.to_owned())),
        );
        let mut out = Vec::new();
        let mut buf = vec![0u8; out_chunk];
        loop {
            let n = r.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        let titles = seen.lock().unwrap().clone();
        (out, titles)
    }

    #[test]
    fn strips_metadata_across_arbitrary_boundaries() {
        let titles = [
            "Daft Punk - One More Time",
            "",
            "Guns N' Roses - Don't Cry",
            "Guns N' Roses - Don't Cry",
            "Ünïcødé",
        ];
        let (wire, audio) = build_stream(37, 6, &titles);
        for steps in [vec![1], vec![2, 3, 5], vec![7, 1, 64], vec![4096]] {
            for out_chunk in [1, 3, 16, 1000] {
                let (out, seen) = run(wire.clone(), 37, steps.clone(), out_chunk);
                assert_eq!(out, audio, "steps {steps:?} chunk {out_chunk}");
                // Empty metadata emits nothing; repeated title emitted once.
                assert_eq!(
                    seen,
                    [
                        "Daft Punk - One More Time",
                        "Guns N' Roses - Don't Cry",
                        "Ünïcødé"
                    ],
                    "steps {steps:?} chunk {out_chunk}"
                );
            }
        }
    }

    #[test]
    fn parses_stream_title_variants() {
        assert_eq!(
            parse_stream_title(b"StreamTitle='A - B';StreamUrl='http://x';\0\0\0").as_deref(),
            Some("A - B")
        );
        assert_eq!(
            parse_stream_title(b"StreamTitle='It's';").as_deref(),
            Some("It's")
        );
        assert_eq!(
            parse_stream_title(b"StreamTitle='No end").as_deref(),
            Some("No end")
        );
        assert_eq!(
            parse_stream_title(b"StreamTitle='Trailing'").as_deref(),
            Some("Trailing")
        );
        assert_eq!(parse_stream_title(b"StreamUrl='x';"), None);
        // Latin-1 fallback.
        assert_eq!(
            parse_stream_title(b"StreamTitle='Caf\xe9';").as_deref(),
            Some("Café")
        );
    }
}
