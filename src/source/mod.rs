mod local;

pub use local::LocalSource;

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

use crate::media::MediaItem;

#[allow(dead_code)]
/// A source of media items (local filesystem, online extractor, etc.).
#[async_trait]
pub trait Source: Send + Sync {
    fn name(&self) -> &str;
    fn supports_search(&self) -> bool;
    fn supports_browse(&self) -> bool;
    async fn search(&self, query: &str, offset: usize, limit: usize) -> Result<Vec<MediaItem>>;
    async fn browse(&self, offset: usize, limit: usize) -> Result<Vec<MediaItem>>;
}

/// Registry of all available sources.
#[allow(dead_code)]
pub struct SourceRegistry {
    pub sources: Vec<Arc<dyn Source>>,
}

#[allow(dead_code)]
impl SourceRegistry {
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    pub fn add(&mut self, source: Arc<dyn Source>) {
        self.sources.push(source);
    }

    pub fn find_searchable(&self) -> Option<&Arc<dyn Source>> {
        self.sources.iter().find(|s| s.supports_search())
    }
}
