use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

pub trait ArticleCache: Send + Sync {
    fn get(&self, message_id: &str) -> Option<Arc<Vec<u8>>>;
    fn put(&self, message_id: String, data: Vec<u8>);
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
    inner: Mutex<CacheInner>,
}

struct CacheInner {
    entries: HashMap<String, Arc<Vec<u8>>>,
    order: VecDeque<String>,
    current_bytes: usize,
}

impl BoundedCache {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            inner: Mutex::new(CacheInner {
                entries: HashMap::new(),
                order: VecDeque::new(),
                current_bytes: 0,
            }),
        }
    }
}

impl ArticleCache for BoundedCache {
    fn get(&self, message_id: &str) -> Option<Arc<Vec<u8>>> {
        let inner = self.inner.lock().unwrap();
        inner.entries.get(message_id).cloned()
    }

    fn put(&self, message_id: String, data: Vec<u8>) {
        let data_len = data.len();
        if data_len > self.max_bytes {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.entries.contains_key(&message_id) {
            return;
        }
        while inner.current_bytes + data_len > self.max_bytes {
            if let Some(oldest) = inner.order.pop_front() {
                if let Some(old_data) = inner.entries.remove(&oldest) {
                    inner.current_bytes -= old_data.len();
                }
            } else {
                break;
            }
        }
        inner.current_bytes += data_len;
        inner.order.push_back(message_id.clone());
        inner.entries.insert(message_id, Arc::new(data));
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
}
