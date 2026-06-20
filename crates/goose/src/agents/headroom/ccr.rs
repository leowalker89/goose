//! CCR (Compress-Cache-Retrieve) storage layer stub for goose headroom integration.
//!
//! Simplified version without the full multi-backend system (redis, sqlite).
//! Just provides in-memory storage for compressed payloads.
#![allow(clippy::string_slice)]

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dashmap::DashMap;

pub trait CcrStore: Send + Sync {
    fn put(&self, hash: &str, payload: &str);
    fn get(&self, hash: &str) -> Option<String>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub const DEFAULT_CAPACITY: usize = 1000;
pub const DEFAULT_TTL: Duration = Duration::from_secs(1800);

#[derive(Clone)]
struct Entry {
    payload: String,
    inserted: Instant,
}

pub struct InMemoryCcrStore {
    map: DashMap<String, Entry>,
    order: Mutex<VecDeque<String>>,
    ttl: Duration,
    capacity: usize,
}

impl InMemoryCcrStore {
    pub fn new() -> Self {
        Self::with_capacity_and_ttl(DEFAULT_CAPACITY, DEFAULT_TTL)
    }

    pub fn with_capacity_and_ttl(capacity: usize, ttl: Duration) -> Self {
        Self {
            map: DashMap::with_capacity(capacity),
            order: Mutex::new(VecDeque::with_capacity(capacity)),
            ttl,
            capacity,
        }
    }

    fn evict_until_under_capacity(&self) {
        let mut guard = self.order.lock().expect("ccr order mutex poisoned");
        while self.map.len() >= self.capacity {
            let Some(oldest) = guard.pop_front() else {
                break;
            };
            self.map.remove(&oldest);
        }
    }
}

impl Default for InMemoryCcrStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CcrStore for InMemoryCcrStore {
    fn put(&self, hash: &str, payload: &str) {
        if let Some(mut existing) = self.map.get_mut(hash) {
            existing.payload = payload.to_string();
            existing.inserted = Instant::now();
            return;
        }

        if self.map.len() >= self.capacity {
            self.evict_until_under_capacity();
        }
        let entry = Entry {
            payload: payload.to_string(),
            inserted: Instant::now(),
        };
        let prev = self.map.insert(hash.to_string(), entry);
        if prev.is_none() {
            self.order
                .lock()
                .expect("ccr order mutex poisoned")
                .push_back(hash.to_string());
        }
    }

    fn get(&self, hash: &str) -> Option<String> {
        if let Some(entry) = self.map.get(hash) {
            if entry.inserted.elapsed() <= self.ttl {
                return Some(entry.payload.clone());
            }
        } else {
            return None;
        }
        let was_removed = self
            .map
            .remove_if(hash, |_, entry| entry.inserted.elapsed() > self.ttl)
            .is_some();
        if was_removed {
            None
        } else {
            self.map.get(hash).map(|e| e.payload.clone())
        }
    }

    fn len(&self) -> usize {
        self.map.len()
    }
}

pub fn compute_key(payload: &[u8]) -> String {
    let h = blake3::hash(payload);
    let hex = h.to_hex();
    hex[..24].to_string()
}

pub fn marker_for(hash: &str) -> String {
    format!("<<ccr:{hash}>>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get() {
        let store = InMemoryCcrStore::new();
        store.put("abc123", "payload");
        assert_eq!(store.get("abc123"), Some("payload".to_string()));
    }

    #[test]
    fn compute_key_is_24_hex() {
        let k = compute_key(b"hello");
        assert_eq!(k.len(), 24);
    }
}
