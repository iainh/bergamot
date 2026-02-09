use std::collections::HashMap;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

use crate::model::DecodedSegment;

pub type CacheKey = (u32, u32);

#[derive(Debug)]
pub struct ArticleCache {
    entries: HashMap<CacheKey, Vec<DecodedSegment>>,
    current_size: usize,
    max_size: usize,
}

impl ArticleCache {
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: HashMap::new(),
            current_size: 0,
            max_size,
        }
    }

    pub fn store(&mut self, nzb_id: u32, file_index: u32, segment: DecodedSegment) {
        self.current_size += segment.data.len();
        self.entries
            .entry((nzb_id, file_index))
            .or_default()
            .push(segment);

        if self.current_size > self.max_size {
            self.evict_largest();
        }
    }

    pub fn flush_file(
        &mut self,
        nzb_id: u32,
        file_index: u32,
        output: &mut File,
    ) -> std::io::Result<()> {
        if let Some(segments) = self.entries.remove(&(nzb_id, file_index)) {
            for seg in &segments {
                let offset = seg.begin.saturating_sub(1);
                output.seek(SeekFrom::Start(offset))?;
                output.write_all(&seg.data)?;
                self.current_size = self.current_size.saturating_sub(seg.data.len());
            }
        }
        Ok(())
    }

    pub fn current_size(&self) -> usize {
        self.current_size
    }

    fn evict_largest(&mut self) {
        if let Some(key) = self
            .entries
            .iter()
            .max_by_key(|(_, segments)| segments.iter().map(|seg| seg.data.len()).sum::<usize>())
            .map(|(key, _)| *key)
        {
            self.entries.remove(&key);
        }
        self.current_size = self
            .entries
            .values()
            .flat_map(|segments| segments.iter())
            .map(|seg| seg.data.len())
            .sum();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_tracks_size_and_flushes() {
        let mut cache = ArticleCache::new(10);
        cache.store(
            1,
            1,
            DecodedSegment {
                begin: 1,
                end: 3,
                data: vec![1, 2, 3],
                crc32: 0,
            },
        );
        assert_eq!(cache.current_size(), 3);
        cache.evict_largest();
        assert_eq!(cache.current_size(), 0);
    }
}
