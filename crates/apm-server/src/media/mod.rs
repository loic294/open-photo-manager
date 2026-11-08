//! Media serving: thumbnails, previews and original-file streaming.
//!
//! Owned by todo `serving-media`. The `GET /api/files/{id}/thumb|preview|content` handlers live in
//! `api/media.rs`; they look the file up by id, resolve it only through
//! [`VolumeRegistry::get`](crate::indexer::VolumeRegistry::get) and
//! [`VolumeRoot::open_read`](crate::security::VolumeRoot::open_read), and delegate to:
//!
//! * [`MediaService`] (`service.rs`, `thumbs.rs`): renders and caches pictures; it implements the
//!   indexer's [`Thumbnailer`](crate::indexer::Thumbnailer), so the pre-generation stage shares
//!   its concurrency limit, single flight and cache.
//! * [`render`]: the [`Renderer`] abstraction (in-process `image`, `vipsthumbnail`, `ffmpeg`,
//!   `exiftool` for RAW) and the one routing table; its docs hold the measured timings.
//! * [`cache`]: the cache layout, atomic writes and the sweep.
//! * [`serve`]: the original file with `Range`, conditional requests and safe headers.
//! * [`preview`]: which images the browser shows as they are and which get a JPEG preview.
//!
//! # The service
//! * **Concurrency.** One gate whose size is `settings.index.thumbnail_concurrency`, read on every
//!   attempt (a changed setting applies within a second) and shared by on-demand requests and the
//!   pre-generation stage. Renders run in spawned tasks (in-process decoding on the blocking pool,
//!   tools as child processes), never on the request's own task.
//! * **Single flight.** Requests for the same [`CacheKey`] wait for one render. A request that is
//!   dropped while it waits for the gate never starts rendering; when the request that renders is
//!   dropped, one of the others takes over.
//! * **Containment.** A panic in a renderer is a failed render (`thumbnail_failed`), not a crashed
//!   task or a hung request.
//! * **Failures.** A failed thumbnail sets `thumb_state = failed` (guarded by `size` and `mtime`)
//!   and is remembered in a bounded in-memory map, so a broken file is rendered once, not on every
//!   request. The scanner makes the row `pending` again when the file changes.
//! * **Stale rows.** When the open file no longer has the `size` and `mtime` of its row the
//!   picture is rendered fresh, sent with `Cache-Control: no-store` and not cached.
//! * **Cache.** `{data_dir}/cache/...`, see [`cache`]. A cache hit needs neither the volume nor the
//!   file: the key comes from the index row.
//!
//! # Status and error codes
//! | status | code | when |
//! |---|---|---|
//! | 400 | `invalid_query` | `size` is not `sm` or `lg` |
//! | 404 | `not_found` | no such file id, or an unknown volume |
//! | 404 | `file_missing` | the row exists but the file is gone from the disk |
//! | 404 | `no_thumbnail` / `no_preview` | audio, sidecar and other files have no picture |
//! | 403 | `symlink_rejected` | a link was put where the file is |
//! | 409 | `volume_offline` | the volume is not mounted |
//! | 416 | | an unsatisfiable `Range` (`Content-Range: bytes */len`) |
//! | 422 | `thumbnail_failed` | the file could not be rendered (`Cache-Control: private, max-age=600`) |

pub mod cache;
mod external;
pub mod preview;
pub mod render;
pub mod serve;
mod service;
pub mod thumbs;

pub use cache::{CacheKey, Variant};
pub use render::{DefaultRenderer, RenderError, RenderRequest, Renderer};
pub use service::MediaService;
pub use thumbs::{MediaError, Rendition, SweepReport};
