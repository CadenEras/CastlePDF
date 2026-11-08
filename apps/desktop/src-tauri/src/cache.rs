//! Least-recently-used cache of tiles in the shell, counted in bytes
//! (ADR-0003: 128 MiB by default, `docs/PERFORMANCE.md`). A tile is the
//! buffer sent to the UI, shared by `Arc` with the requests it answers; its
//! bytes are counted once, while it is cached.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use pdf_worker::protocol::TileRect;

pub const DEFAULT_CAPACITY: usize = 128 * 1024 * 1024;

/// What identifies a tile: document, page, scale (exact bits) and rectangle.
/// The document id changes when a file is opened again; rotation and colour
/// profile are not view options yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub document: u32,
    pub page: u32,
    scale_bits: u64,
    x: u32,
    y: u32,
    width: u16,
    height: u16,
}

impl TileKey {
    pub fn new(document: u32, page: u32, scale: f64, rect: TileRect) -> Self {
        Self {
            document,
            page,
            scale_bits: scale.to_bits(),
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }

    pub fn scale(&self) -> f64 {
        f64::from_bits(self.scale_bits)
    }

    pub fn rect(&self) -> TileRect {
        TileRect {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

pub struct TileCache {
    capacity: usize,
    bytes: usize,
    /// Tile and its last use.
    entries: HashMap<TileKey, (Arc<Vec<u8>>, u64)>,
    /// Keys by last use, oldest first.
    order: BTreeMap<u64, TileKey>,
    clock: u64,
}

impl Default for TileCache {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl TileCache {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            bytes: 0,
            entries: HashMap::new(),
            order: BTreeMap::new(),
            clock: 0,
        }
    }

    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    pub fn get(&mut self, key: &TileKey) -> Option<Arc<Vec<u8>>> {
        let now = self.tick();
        let (tile, used) = self.entries.get_mut(key)?;
        self.order.remove(used);
        *used = now;
        self.order.insert(now, *key);
        Some(Arc::clone(tile))
    }

    /// Stores `tile`, evicting the least recently used tiles to stay within
    /// the capacity. A tile larger than the capacity is not stored.
    pub fn insert(&mut self, key: TileKey, tile: Arc<Vec<u8>>) {
        self.remove(&key);
        let size = tile.len();
        if size > self.capacity {
            return;
        }
        while self.bytes + size > self.capacity {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some((evicted, _)) = self.entries.remove(&oldest) {
                self.bytes -= evicted.len();
            }
        }
        let now = self.tick();
        self.order.insert(now, key);
        self.entries.insert(key, (tile, now));
        self.bytes += size;
    }

    fn remove(&mut self, key: &TileKey) {
        if let Some((tile, used)) = self.entries.remove(key) {
            self.order.remove(&used);
            self.bytes -= tile.len();
        }
    }

    pub fn remove_document(&mut self, document: u32) {
        let keys: Vec<TileKey> = self
            .entries
            .keys()
            .filter(|key| key.document == document)
            .copied()
            .collect();
        for key in keys {
            self.remove(&key);
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(document: u32, x: u32) -> TileKey {
        TileKey::new(
            document,
            0,
            1.0,
            TileRect {
                x,
                y: 0,
                width: 1,
                height: 1,
            },
        )
    }

    fn tile(len: usize) -> Arc<Vec<u8>> {
        Arc::new(vec![0; len])
    }

    #[test]
    fn least_recently_used_tiles_are_evicted_first() {
        let mut cache = TileCache::with_capacity(30);
        cache.insert(key(1, 0), tile(10));
        cache.insert(key(1, 1), tile(10));
        cache.insert(key(1, 2), tile(10));
        assert_eq!(cache.bytes(), 30);
        // Using the first tile makes the second one the oldest.
        assert!(cache.get(&key(1, 0)).is_some());
        cache.insert(key(1, 3), tile(10));
        assert_eq!(cache.bytes(), 30);
        assert!(cache.get(&key(1, 1)).is_none());
        for x in [0, 2, 3] {
            assert!(cache.get(&key(1, x)).is_some(), "tile {x}");
        }
        // A larger tile evicts as many as needed.
        cache.insert(key(1, 4), tile(25));
        assert_eq!(cache.bytes(), 25);
        assert!(cache.get(&key(1, 4)).is_some());
    }

    #[test]
    fn capacity_is_exact_and_oversized_tiles_are_not_kept() {
        let mut cache = TileCache::with_capacity(10);
        cache.insert(key(1, 0), tile(10));
        assert_eq!(cache.bytes(), 10);
        cache.insert(key(1, 1), tile(11));
        assert!(cache.get(&key(1, 1)).is_none());
        assert!(cache.get(&key(1, 0)).is_some());
        // Replacing a key counts its bytes once.
        cache.insert(key(1, 0), tile(4));
        cache.insert(key(1, 0), tile(6));
        assert_eq!(cache.bytes(), 6);
    }

    #[test]
    fn a_closed_document_leaves_the_cache() {
        let mut cache = TileCache::with_capacity(100);
        cache.insert(key(1, 0), tile(10));
        cache.insert(key(2, 0), tile(10));
        cache.insert(key(1, 1), tile(10));
        cache.remove_document(1);
        assert_eq!(cache.bytes(), 10);
        assert!(cache.get(&key(2, 0)).is_some());
        cache.clear();
        assert_eq!(cache.bytes(), 0);
        assert!(cache.get(&key(2, 0)).is_none());
    }

    #[test]
    fn scales_are_told_apart_exactly() {
        let rect = key(1, 0).rect();
        assert_ne!(
            TileKey::new(1, 0, 1.0, rect),
            TileKey::new(1, 0, 1.0 + f64::EPSILON, rect)
        );
        assert_eq!(TileKey::new(1, 0, 1.5, rect).scale(), 1.5);
    }
}
