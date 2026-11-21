//! Byte budgets for the preview and texture caches.
// pattern: Functional Core

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Share of physical RAM given to the caches when no override is set.
const RAM_DIVISOR: u64 = 4;
/// Textures live in GPU memory, which integrated GPUs share with system RAM
/// and which is far smaller than system RAM on those machines.
pub(crate) const TEXTURE_CAP: u64 = 2 * GIB;
/// Rough size of one preview's bytes, used to estimate the item count.
pub(crate) const PREVIEW_ESTIMATE_BYTES: u64 = 64 * 1024;
/// Size of one 640x424 RGBA texture, used to estimate the item count.
pub(crate) const TEXTURE_ESTIMATE_BYTES: u64 = 640 * 424 * 4;

/// The override in bytes when set, else a quarter of physical RAM.
pub(crate) fn cache_budget_bytes(physical_ram: u64, override_mb: Option<u64>) -> u64 {
    match override_mb {
        Some(mb) => mb.saturating_mul(MIB),
        None => physical_ram / RAM_DIVISOR,
    }
}

/// How a budget is shared between the two caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CacheSplit {
    pub bytes: u64,
    pub textures: u64,
}

/// 20% of `budget` to preview bytes; textures get the rest, capped at
/// `TEXTURE_CAP`. The sum never exceeds `budget`.
pub(crate) fn split(budget: u64) -> CacheSplit {
    // Integer division floors the 20% share. Textures take the remainder,
    // so the shares add up to `budget` unless the texture cap applies.
    let bytes = budget / 5;
    let textures = (budget - bytes).min(TEXTURE_CAP);
    CacheSplit { bytes, textures }
}

/// Item-count hint for `quick_cache`'s `estimated_items_capacity`.
pub(crate) fn preview_items(bytes: u64) -> usize {
    items(bytes, PREVIEW_ESTIMATE_BYTES)
}

/// Item-count hint for `quick_cache`'s `estimated_items_capacity`.
pub(crate) fn texture_items(bytes: u64) -> usize {
    items(bytes, TEXTURE_ESTIMATE_BYTES)
}

fn items(bytes: u64, item_bytes: u64) -> usize {
    usize::try_from(bytes / item_bytes).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn default_budget_is_a_quarter_of_physical_ram() {
        assert_eq!(cache_budget_bytes(24 * GIB, None), 6 * GIB);
    }

    #[test]
    fn override_in_mb_is_used_whatever_the_ram() {
        assert_eq!(cache_budget_bytes(24 * GIB, Some(512)), 512 * MIB);
        assert_eq!(cache_budget_bytes(64 * GIB, Some(512)), 512 * MIB);
    }

    #[test]
    fn split_of_a_16_gib_budget_caps_textures_at_2_gib() {
        // 64 GiB of RAM gives a 16 GiB budget.
        let budget = cache_budget_bytes(64 * GIB, None);
        assert_eq!(budget, 16 * GIB);
        let split = split(budget);
        // 20% of 16 GiB is 3.2 GiB = 3_435_973_836.8 bytes, floored.
        assert_eq!(split.bytes, 3_435_973_836);
        assert_eq!(split.textures, 2 * GIB);
    }

    /// 0.2 GiB = 214_748_364.8 bytes, floored to 214_748_364. Textures take
    /// the remainder, 858_993_460 bytes (0.8 GiB rounded up), so the two
    /// shares sum to exactly the budget.
    #[test]
    fn split_of_a_1_gib_budget_is_0_2_and_0_8_gib() {
        let split = split(GIB);
        assert_eq!(split.bytes, 214_748_364);
        assert_eq!(split.textures, 858_993_460);
        assert_eq!(split.bytes + split.textures, GIB);
    }

    #[test]
    fn item_estimates_divide_the_budget_by_the_expected_item_size() {
        assert_eq!(preview_items(16 * MIB), 256);
        assert_eq!(texture_items(TEXTURE_ESTIMATE_BYTES * 10), 10);
    }

    proptest! {
        #[test]
        fn split_never_exceeds_the_budget(budget in any::<u64>()) {
            let split = split(budget);
            prop_assert!(split.bytes + split.textures <= budget);
            prop_assert!(split.textures <= TEXTURE_CAP);
        }
    }
}
