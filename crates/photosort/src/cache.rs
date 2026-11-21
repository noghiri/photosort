//! Byte-budgeted preview and texture caches, and physical RAM detection.
// pattern: Imperative Shell

use std::sync::Arc;

use eframe::egui;
use photosort_core::model::ShotId;
use quick_cache::{Weighter, sync, unsync};

use crate::budget::{CacheSplit, preview_items, texture_items};

/// Total physical RAM in bytes, or `None` if the OS does not report it.
#[cfg(windows)]
pub(crate) fn physical_ram() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX {
        dwLength: u32::try_from(std::mem::size_of::<MEMORYSTATUSEX>()).ok()?,
        dwMemoryLoad: 0,
        ullTotalPhys: 0,
        ullAvailPhys: 0,
        ullTotalPageFile: 0,
        ullAvailPageFile: 0,
        ullTotalVirtual: 0,
        ullAvailVirtual: 0,
        ullAvailExtendedVirtual: 0,
    };
    // SAFETY: `status` is zero-initialised, and its `dwLength` is set to its
    // own size before the call, as the API requires. The pointer comes from
    // a live `&mut` borrow that outlives the call, and the API writes only
    // within the struct.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    (ok != 0 && status.ullTotalPhys > 0).then_some(status.ullTotalPhys)
}

/// Weighs a preview by its byte length.
#[derive(Clone, Default)]
pub(crate) struct ByteWeighter;

impl Weighter<ShotId, Arc<[u8]>> for ByteWeighter {
    fn weight(&self, _key: &ShotId, val: &Arc<[u8]>) -> u64 {
        u64::try_from(val.len()).unwrap_or(u64::MAX)
    }
}

/// Weighs a texture by its RGBA size, `w * h * 4`.
#[derive(Clone, Default)]
pub(crate) struct TextureWeighter;

impl Weighter<ShotId, egui::TextureHandle> for TextureWeighter {
    fn weight(&self, _key: &ShotId, val: &egui::TextureHandle) -> u64 {
        let [w, h] = val.size();
        let w = u64::try_from(w).unwrap_or(u64::MAX);
        let h = u64::try_from(h).unwrap_or(u64::MAX);
        w.saturating_mul(h).saturating_mul(4)
    }
}

/// Encoded preview bytes, shared between the scanner's workers and the UI.
pub(crate) type PreviewBytes = sync::Cache<ShotId, Arc<[u8]>, ByteWeighter>;

/// Uploaded textures. UI-thread only.
pub(crate) type Textures = unsync::Cache<ShotId, egui::TextureHandle, TextureWeighter>;

/// A preview-bytes cache holding at most `split.bytes`.
pub(crate) fn preview_bytes(split: &CacheSplit) -> PreviewBytes {
    sync::Cache::with_weighter(preview_items(split.bytes), split.bytes, ByteWeighter)
}

/// A texture cache holding at most `split.textures`.
pub(crate) fn textures(split: &CacheSplit) -> Textures {
    unsync::Cache::with_weighter(
        texture_items(split.textures),
        split.textures,
        TextureWeighter,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;
    const KIB: usize = 1024;

    /// quick_cache 0.7.0 uses S3-FIFO, not LRU, so which early keys go is
    /// policy-dependent. The test asserts only what the policy guarantees:
    /// the byte budget holds, most entries are evicted, and the newest
    /// entry is kept.
    #[test]
    fn preview_cache_evicts_to_stay_within_its_budget() {
        let cache = preview_bytes(&CacheSplit {
            bytes: MIB,
            textures: 0,
        });
        let entry: Arc<[u8]> = Arc::from(vec![0u8; 64 * KIB]);
        for i in 0..100 {
            cache.insert(ShotId(i), Arc::clone(&entry));
        }
        assert!(cache.weight() <= MIB);
        let evicted = (0..100)
            .filter(|&i| cache.get(&ShotId(i)).is_none())
            .count();
        assert!(evicted >= 84, "only {evicted} of 100 entries were evicted");
        assert!(cache.get(&ShotId(99)).is_some());
    }

    #[test]
    fn texture_weight_is_width_times_height_times_four() {
        let ctx = egui::Context::default();
        let image = egui::ColorImage::filled([640, 424], egui::Color32::BLACK);
        let handle = ctx.load_texture("weight-test", image, egui::TextureOptions::LINEAR);
        assert_eq!(TextureWeighter.weight(&ShotId(0), &handle), 640 * 424 * 4);
    }

    #[test]
    fn texture_cache_stays_within_its_weight_budget() {
        let ctx = egui::Context::default();
        let mut cache = textures(&CacheSplit {
            bytes: 0,
            textures: 2 * MIB,
        });
        for i in 0..3 {
            let image = egui::ColorImage::filled([640, 424], egui::Color32::BLACK);
            let handle = ctx.load_texture(format!("t{i}"), image, egui::TextureOptions::LINEAR);
            cache.insert(ShotId(i), handle);
        }
        assert!(cache.weight() <= 2 * MIB);
        assert!(cache.len() < 3);
    }

    #[cfg(windows)]
    #[test]
    fn physical_ram_reports_a_positive_size() {
        assert!(physical_ram().is_some_and(|n| n > 0));
    }
}
