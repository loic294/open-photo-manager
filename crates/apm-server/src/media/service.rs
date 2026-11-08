//! [`MediaService`]: the state shared by every render (renderer, cache, gate, flights, failures).
//! Its behaviour is documented in the `media` module docs; the render path is in `thumbs.rs`.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{Notify, OnceCell};

use super::cache::{Cache, CacheKey};
use super::render::{DefaultRenderer, Renderer};
use super::thumbs::RenderFailure;
use crate::db::Db;
use crate::indexer::VolumeRegistry;
use crate::settings::SettingsService;

/// The most failed renders remembered at a time (oldest forgotten first).
const FAILURE_MEMORY: usize = 4096;

/// Renders and caches the pictures of the library, and is the one place that serves them. Cheap
/// to clone.
#[derive(Clone)]
pub struct MediaService {
    pub(super) inner: Arc<Inner>,
}

pub(super) struct Inner {
    pub(super) db: Db,
    pub(super) volumes: VolumeRegistry,
    pub(super) settings: SettingsService,
    pub(super) cache: Arc<Cache>,
    pub(super) renderer: Arc<dyn Renderer>,
    pub(super) gate: Arc<Gate>,
    pub(super) flights: Mutex<HashMap<CacheKey, Arc<Flight>>>,
    pub(super) failures: Mutex<Failures>,
    pub(super) cache_warned: AtomicBool,
    pub(super) sweep_cursor: AtomicUsize,
}

impl fmt::Debug for MediaService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaService").finish_non_exhaustive()
    }
}

impl MediaService {
    /// The production service: the default renderer, a cache and a private `HOME` for the tools
    /// below `data_dir`. Empties `cache/tmp` (partial renders of a run that was killed).
    pub fn new(
        db: Db,
        volumes: VolumeRegistry,
        settings: SettingsService,
        data_dir: PathBuf,
    ) -> Self {
        let cache = Cache::open(&data_dir);
        let home = data_dir.join("tmp").join("media-home");
        if let Err(err) = std::fs::create_dir_all(&home) {
            tracing::warn!(error = %err, "cannot create the home directory of the media tools");
        }
        let renderer = DefaultRenderer::new(cache.tmp_dir().to_path_buf(), home);
        Self::build(db, volumes, settings, Arc::new(cache), Arc::new(renderer))
    }

    /// The same service (sharing the cache) rendering with `renderer`; tests inject a
    /// controllable one.
    #[must_use]
    pub fn with_renderer(&self, renderer: Arc<dyn Renderer>) -> Self {
        let inner = &self.inner;
        Self::build(
            inner.db.clone(),
            inner.volumes.clone(),
            inner.settings.clone(),
            Arc::clone(&inner.cache),
            renderer,
        )
    }

    fn build(
        db: Db,
        volumes: VolumeRegistry,
        settings: SettingsService,
        cache: Arc<Cache>,
        renderer: Arc<dyn Renderer>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                volumes,
                settings,
                cache,
                renderer,
                gate: Arc::new(Gate::default()),
                flights: Mutex::new(HashMap::new()),
                failures: Mutex::new(Failures::default()),
                cache_warned: AtomicBool::new(false),
                sweep_cursor: AtomicUsize::new(0),
            }),
        }
    }

    /// Places of the render gate in use right now (tests).
    #[doc(hidden)]
    pub fn renders_in_flight(&self) -> usize {
        self.inner.gate.in_use()
    }
}

/// One render in progress; its result is shared by every request for the same key.
pub(super) type Flight = OnceCell<Result<bytes::Bytes, RenderFailure>>;

/// A counting gate whose size is read on every attempt. Waiting is cancel-safe: a dropped waiter
/// holds nothing.
#[derive(Default)]
pub(super) struct Gate {
    in_use: Mutex<usize>,
    freed: Notify,
}

/// Holds one place of the gate until dropped.
pub(super) struct Permit {
    gate: Arc<Gate>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        *self
            .gate
            .in_use
            .lock()
            .unwrap_or_else(PoisonError::into_inner) -= 1;
        self.gate.freed.notify_waiters();
    }
}

impl Gate {
    /// Waits until fewer than `limit()` places are taken, then takes one. `limit` is asked again
    /// on every attempt, so a changed setting applies to the next waiter.
    pub(super) async fn acquire(gate: &Arc<Self>, limit: impl Fn() -> usize) -> Permit {
        loop {
            let notified = gate.freed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut in_use = gate.in_use.lock().unwrap_or_else(PoisonError::into_inner);
                if *in_use < limit().max(1) {
                    *in_use += 1;
                    return Permit {
                        gate: Arc::clone(gate),
                    };
                }
            }
            tokio::select! {
                () = &mut notified => {}
                () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            }
        }
    }

    fn in_use(&self) -> usize {
        *self.in_use.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The renders that failed, oldest first, bounded.
#[derive(Default)]
pub(super) struct Failures {
    messages: HashMap<CacheKey, String>,
    order: VecDeque<CacheKey>,
}

impl Failures {
    pub(super) fn get(&self, key: &CacheKey) -> Option<String> {
        self.messages.get(key).cloned()
    }

    pub(super) fn insert(&mut self, key: CacheKey, message: String) {
        if self.messages.insert(key, message).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > FAILURE_MEMORY {
            if let Some(oldest) = self.order.pop_front() {
                self.messages.remove(&oldest);
            }
        }
    }
}
