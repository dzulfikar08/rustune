mod ytdlp;

pub use ytdlp::YtdlpExtractor;

use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

use crate::media::{MediaItem, StreamInfo};

/// Availability status of an extractor tool.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ExtractorStatus {
    Available(String), // version string
    NotFound,
    Broken(String), // error message
}

/// An extractor resolves media IDs to playable stream URLs and can search.
pub trait Extractor: Send + Sync {
    fn name(&self) -> &str;
    fn status(&self) -> ExtractorStatus;
    fn resolve(&self, id: &str, title: &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<StreamInfo>> + Send + '_>>;
    fn search(&self, query: &str, offset: usize, limit: usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<MediaItem>>> + Send + '_>>;
}

/// Registry of available extractors.
pub struct ExtractorRegistry {
    extractors: HashMap<String, Arc<dyn Extractor>>,
}

impl ExtractorRegistry {
    pub fn new() -> Self {
        Self {
            extractors: HashMap::new(),
        }
    }

    pub fn add(&mut self, extractor: Arc<dyn Extractor>) {
        self.extractors.insert(extractor.name().to_string(), extractor);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Extractor>> {
        self.extractors.get(name)
    }

    pub fn first_available(&self) -> Option<&Arc<dyn Extractor>> {
        self.extractors.values().find(|e| {
            matches!(e.status(), ExtractorStatus::Available(_))
        })
    }

    #[allow(dead_code)]
    pub fn all_statuses(&self) -> Vec<(&str, ExtractorStatus)> {
        self.extractors
            .iter()
            .map(|(name, e)| (name.as_str(), e.status()))
            .collect()
    }
}
