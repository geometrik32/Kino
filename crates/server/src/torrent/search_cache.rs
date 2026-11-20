//! Remembers what the indexer found for a title for a while, so opening the
//! release list right after the title page (which starts the search in the
//! background) costs nothing, and repeated visits do not hit every tracker again.
//!
//! The cache is small by design: entries expire after [`TTL`], and at most
//! [`MAX_ENTRIES`] are kept (the oldest go first).

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use kino_core::MediaKind;
use tokio::sync::OnceCell;

use super::torznab::TorznabItem;

/// How long a search result is reused.
pub const TTL: Duration = Duration::from_secs(15 * 60);
/// How many titles are remembered at once.
pub const MAX_ENTRIES: usize = 100;

type Items = Arc<Vec<TorznabItem>>;

struct Entry {
    started: Instant,
    /// Filled once by whoever searches first; others wait for the same result.
    cell: Arc<OnceCell<Items>>,
}

pub struct SearchCache {
    ttl: Duration,
    max: usize,
    entries: Mutex<HashMap<(MediaKind, i64), Entry>>,
}

impl Default for SearchCache {
    fn default() -> Self {
        Self::new(TTL, MAX_ENTRIES)
    }
}

impl SearchCache {
    pub fn new(ttl: Duration, max: usize) -> Self {
        Self {
            ttl,
            max: max.max(1),
            entries: Mutex::default(),
        }
    }

    /// The cached result for the title, or the result of `search`, which runs
    /// once even when several callers ask at the same time. A failed search is
    /// not remembered: the next caller tries again.
    pub async fn get_or_search<F, Fut, E>(&self, key: (MediaKind, i64), search: F) -> Result<Items, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Items, E>>,
    {
        let cell = {
            let mut entries = self.entries.lock().expect("search cache lock");
            entries.retain(|_, e| e.started.elapsed() < self.ttl);
            if !entries.contains_key(&key) && entries.len() >= self.max {
                if let Some(oldest) = entries.iter().min_by_key(|(_, e)| e.started).map(|(k, _)| *k) {
                    entries.remove(&oldest);
                }
            }
            entries
                .entry(key)
                .or_insert_with(|| Entry {
                    started: Instant::now(),
                    cell: Arc::new(OnceCell::new()),
                })
                .cell
                .clone()
        };
        cell.get_or_try_init(search).await.cloned()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    const KEY: (MediaKind, i64) = (MediaKind::Movie, 1);

    fn item(title: &str) -> Items {
        Arc::new(vec![TorznabItem {
            title: title.into(),
            ..TorznabItem::default()
        }])
    }

    #[tokio::test]
    async fn searches_once_even_when_asked_together() {
        let cache = SearchCache::default();
        let runs = AtomicU32::new(0);
        let search = || async {
            runs.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(30)).await;
            Ok::<_, ()>(item("a"))
        };
        let (a, b, c) = tokio::join!(
            cache.get_or_search(KEY, search),
            cache.get_or_search(KEY, search),
            cache.get_or_search(KEY, search)
        );
        assert!(a.is_ok() && b.is_ok() && c.is_ok());
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // Later too.
        cache.get_or_search(KEY, search).await.unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failures_are_not_remembered() {
        let cache = SearchCache::default();
        assert!(cache
            .get_or_search(KEY, || async { Err::<Items, _>("down") })
            .await
            .is_err());
        let ok = cache.get_or_search(KEY, || async { Ok::<_, &str>(item("a")) }).await;
        assert_eq!(ok.unwrap()[0].title, "a");
    }

    #[tokio::test]
    async fn entries_expire() {
        let cache = SearchCache::new(Duration::from_millis(20), 10);
        cache
            .get_or_search(KEY, || async { Ok::<_, ()>(item("old")) })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let fresh = cache
            .get_or_search(KEY, || async { Ok::<_, ()>(item("new")) })
            .await
            .unwrap();
        assert_eq!(fresh[0].title, "new");
    }

    #[tokio::test]
    async fn size_is_capped_and_the_oldest_go_first() {
        let cache = SearchCache::new(TTL, 3);
        for id in 0..10 {
            cache
                .get_or_search((MediaKind::Movie, id), || async { Ok::<_, ()>(item("x")) })
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
            assert!(cache.len() <= 3);
        }
        // The last three are still there: no new search for them.
        let runs = AtomicU32::new(0);
        for id in 7..10 {
            cache
                .get_or_search((MediaKind::Movie, id), || async {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ()>(item("again"))
                })
                .await
                .unwrap();
        }
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }
}
