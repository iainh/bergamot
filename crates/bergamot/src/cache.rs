use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dashmap::DashMap;

pub trait ArticleCache: Send + Sync {
    fn get(&self, message_id: &str) -> Option<Arc<Vec<u8>>>;
    fn put(&self, message_id: String, data: Vec<u8>);
    fn put_arc(&self, message_id: String, data: Arc<Vec<u8>>) {
        self.put(message_id, (*data).clone());
    }
}

pub struct NoopCache;

impl ArticleCache for NoopCache {
    fn get(&self, _message_id: &str) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn put(&self, _message_id: String, _data: Vec<u8>) {}
}

pub struct BoundedCache {
    max_bytes: usize,
    entries: DashMap<String, Arc<Vec<u8>>>,
    eviction: Mutex<VecDeque<String>>,
    current_bytes: Arc<AtomicUsize>,
}

impl BoundedCache {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            entries: DashMap::new(),
            eviction: Mutex::new(VecDeque::new()),
            current_bytes: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn current_bytes_ref(&self) -> &Arc<AtomicUsize> {
        &self.current_bytes
    }
}

impl ArticleCache for BoundedCache {
    fn get(&self, message_id: &str) -> Option<Arc<Vec<u8>>> {
        self.entries.get(message_id).map(|r| r.value().clone())
    }

    fn put(&self, message_id: String, data: Vec<u8>) {
        self.put_arc(message_id, Arc::new(data));
    }

    fn put_arc(&self, message_id: String, data: Arc<Vec<u8>>) {
        let data_len = data.len();
        if data_len > self.max_bytes {
            return;
        }
        if self.entries.contains_key(&message_id) {
            return;
        }
        let mut order = self.eviction.lock().unwrap();
        while self.current_bytes.load(Ordering::Relaxed) + data_len > self.max_bytes {
            if let Some(oldest) = order.pop_front() {
                if let Some((_, old_data)) = self.entries.remove(&oldest) {
                    self.current_bytes
                        .fetch_sub(old_data.len(), Ordering::Relaxed);
                }
            } else {
                break;
            }
        }
        self.current_bytes.fetch_add(data_len, Ordering::Relaxed);
        order.push_back(message_id.clone());
        self.entries.insert(message_id, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_cache_returns_none() {
        let cache = NoopCache;
        assert!(cache.get("test@example").is_none());
    }

    #[test]
    fn bounded_cache_stores_and_retrieves() {
        let cache = BoundedCache::new(1024);
        cache.put("msg1".to_string(), vec![1, 2, 3]);
        let result = cache.get("msg1");
        assert_eq!(result.as_deref(), Some(&vec![1, 2, 3]));
    }

    #[test]
    fn bounded_cache_evicts_oldest_when_full() {
        let cache = BoundedCache::new(10);
        cache.put("a".to_string(), vec![0; 6]);
        cache.put("b".to_string(), vec![1; 6]);
        assert!(cache.get("a").is_none());
        assert_eq!(cache.get("b").as_deref(), Some(&vec![1; 6]));
    }

    #[test]
    fn bounded_cache_rejects_data_larger_than_max() {
        let cache = BoundedCache::new(5);
        cache.put("big".to_string(), vec![0; 10]);
        assert!(cache.get("big").is_none());
    }

    #[test]
    fn bounded_cache_dedup_same_key() {
        let cache = BoundedCache::new(1024);
        cache.put("msg".to_string(), vec![1, 2, 3]);
        cache.put("msg".to_string(), vec![4, 5, 6]);
        assert_eq!(cache.get("msg").as_deref(), Some(&vec![1, 2, 3]));
    }

    #[test]
    fn bounded_cache_concurrent_access() {
        use std::thread;

        let cache = Arc::new(BoundedCache::new(10_000));
        let mut handles = Vec::new();

        for t in 0..4 {
            let c = cache.clone();
            handles.push(thread::spawn(move || {
                for i in 0..100 {
                    let key = format!("t{t}-msg{i}");
                    c.put(key.clone(), vec![t as u8; 10]);
                    c.get(&key);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }
    }
}
