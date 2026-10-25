// Native Rust pixel/arena allocations are scoped VM mappings above 4 MiB.
// Small allocations and platforms other than macOS retain System's behavior.
use std::alloc::{GlobalAlloc, Layout, System};

const THRESHOLD: usize = 4 * 1024 * 1024;

#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    pub enabled: bool,
    pub threshold_bytes: usize,
    pub current_bytes: usize,
    pub peak_bytes: usize,
    pub map_calls: usize,
    pub unmap_calls: usize,
    pub map_failures: usize,
    pub unmap_failures: usize,
}

#[cfg(target_os = "macos")]
mod mapping {
    use super::*;
    use std::ffi::{c_char, c_int, c_void};
    use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering::Relaxed};

    static CONFIG: AtomicU8 = AtomicU8::new(0);
    static CURRENT: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);
    static MAPS: AtomicUsize = AtomicUsize::new(0);
    static UNMAPS: AtomicUsize = AtomicUsize::new(0);
    static MAP_FAILURES: AtomicUsize = AtomicUsize::new(0);
    static UNMAP_FAILURES: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" {
        fn mmap(address: *mut c_void, length: usize, protection: c_int, flags: c_int, fd: c_int, offset: i64) -> *mut c_void;
        fn munmap(address: *mut c_void, length: usize) -> c_int;
        fn getpagesize() -> c_int;
        fn getenv(name: *const c_char) -> *const c_char;
    }

    fn enabled() -> bool {
        let config = CONFIG.load(Relaxed);
        if config != 0 {
            return config == 1;
        }
        // No Rust env/OnceLock calls here: their initialization can allocate.
        // The process launch environment is fixed before loading the addon.
        let value = unsafe { getenv(c"KYBERN_LARGE_ALLOCATIONS_VM".as_ptr()) };
        let enabled = value.is_null() || unsafe { *value } != b'0' as c_char;
        CONFIG.store(if enabled { 1 } else { 2 }, Relaxed);
        enabled
    }

    #[inline]
    fn mapped(layout: Layout) -> bool {
        layout.size() >= THRESHOLD && enabled() && layout.align() <= unsafe { getpagesize() } as usize
    }

    unsafe fn allocate(layout: Layout) -> *mut u8 {
        // Darwin: PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANON. Anonymous maps
        // are zero-filled and their base meets every alignment up to a page.
        let pointer = unsafe { mmap(std::ptr::null_mut(), layout.size(), 3, 0x1002, -1, 0) };
        if pointer == (-1isize) as *mut c_void {
            MAP_FAILURES.fetch_add(1, Relaxed);
            return std::ptr::null_mut();
        }
        let bytes = CURRENT.fetch_add(layout.size(), Relaxed) + layout.size();
        PEAK.fetch_max(bytes, Relaxed);
        MAPS.fetch_add(1, Relaxed);
        pointer.cast()
    }

    pub(super) struct Allocator;

    // The route depends only on the immutable process configuration and the
    // exact allocation layout. No headers, locks, collections, or idle work.
    unsafe impl GlobalAlloc for Allocator {
        #[inline]
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if mapped(layout) {
                unsafe { allocate(layout) }
            } else {
                unsafe { System.alloc(layout) }
            }
        }

        #[inline]
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            if mapped(layout) {
                unsafe { allocate(layout) }
            } else {
                unsafe { System.alloc_zeroed(layout) }
            }
        }

        #[inline]
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            if mapped(layout) {
                if unsafe { munmap(pointer.cast(), layout.size()) } == 0 {
                    CURRENT.fetch_sub(layout.size(), Relaxed);
                    UNMAPS.fetch_add(1, Relaxed);
                } else {
                    UNMAP_FAILURES.fetch_add(1, Relaxed);
                }
            } else {
                unsafe { System.dealloc(pointer, layout) }
            }
        }

        #[inline]
        unsafe fn realloc(&self, pointer: *mut u8, old: Layout, size: usize) -> *mut u8 {
            let new = unsafe { Layout::from_size_align_unchecked(size, old.align()) };
            if !mapped(old) && !mapped(new) {
                // Preserve System's optimized realloc for the common small path.
                return unsafe { System.realloc(pointer, old, size) };
            }
            if old.size() == size {
                return pointer;
            }
            let next = unsafe { self.alloc(new) };
            if !next.is_null() {
                unsafe {
                    std::ptr::copy_nonoverlapping(pointer, next, old.size().min(size));
                    self.dealloc(pointer, old);
                }
            }
            next
        }
    }

    pub(super) fn snapshot() -> Snapshot {
        Snapshot {
            enabled: enabled(),
            threshold_bytes: THRESHOLD,
            current_bytes: CURRENT.load(Relaxed),
            peak_bytes: PEAK.load(Relaxed),
            map_calls: MAPS.load(Relaxed),
            unmap_calls: UNMAPS.load(Relaxed),
            map_failures: MAP_FAILURES.load(Relaxed),
            unmap_failures: UNMAP_FAILURES.load(Relaxed),
        }
    }
}

#[cfg(target_os = "macos")]
#[global_allocator]
static ALLOCATOR: mapping::Allocator = mapping::Allocator;

pub(crate) fn snapshot() -> Snapshot {
    #[cfg(target_os = "macos")]
    {
        mapping::snapshot()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Snapshot { threshold_bytes: THRESHOLD, ..Snapshot::default() }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::alloc::{alloc, alloc_zeroed, dealloc, realloc};

    #[test]
    fn large_buffers_are_zeroed_aligned_and_released() {
        let before = snapshot().current_bytes;
        let layout = Layout::from_size_align(THRESHOLD + 128, 64).unwrap();
        unsafe {
            let pointer = alloc_zeroed(layout);
            assert!(!pointer.is_null());
            assert_eq!(pointer as usize % 64, 0);
            assert_eq!(*pointer, 0);
            assert_eq!(*pointer.add(layout.size() - 1), 0);
            assert_eq!(snapshot().current_bytes, before + layout.size());
            dealloc(pointer, layout);
        }
        assert_eq!(snapshot().current_bytes, before);
    }

    #[test]
    fn realloc_crosses_the_threshold_in_both_directions() {
        let before = snapshot().current_bytes;
        let small = Layout::from_size_align(THRESHOLD - 16, 16).unwrap();
        unsafe {
            let pointer = alloc_zeroed(small);
            assert!(!pointer.is_null());
            *pointer = 83;
            *pointer.add(small.size() - 1) = 127;
            let size = THRESHOLD + 128;
            let large = realloc(pointer, small, size);
            assert!(!large.is_null());
            assert_eq!(*large, 83);
            assert_eq!(*large.add(small.size() - 1), 127);
            assert_eq!(snapshot().current_bytes, before + size);
            let shrunk = realloc(large, Layout::from_size_align(size, 16).unwrap(), 1024);
            assert!(!shrunk.is_null());
            assert_eq!(*shrunk, 83);
            assert_eq!(snapshot().current_bytes, before);
            dealloc(shrunk, Layout::from_size_align(1024, 16).unwrap());
        }
    }

    #[test]
    fn small_and_overaligned_buffers_use_system() {
        let maps = snapshot().map_calls;
        for layout in [Layout::from_size_align(1024, 16).unwrap(), Layout::from_size_align(THRESHOLD, 65536).unwrap()] {
            unsafe {
                let pointer = alloc(layout);
                assert!(!pointer.is_null());
                assert_eq!(pointer as usize % layout.align(), 0);
                *pointer = 61;
                assert_eq!(*pointer, 61);
                dealloc(pointer, layout);
            }
        }
        assert_eq!(snapshot().map_calls, maps);
    }

    #[test]
    #[ignore = "separate process with KYBERN_LARGE_ALLOCATIONS_VM=0"]
    fn disabled_control_uses_system() {
        assert!(!snapshot().enabled);
        let maps = snapshot().map_calls;
        let layout = Layout::from_size_align(THRESHOLD + 128, 16).unwrap();
        unsafe {
            let pointer = alloc_zeroed(layout);
            assert!(!pointer.is_null());
            assert_eq!(*pointer, 0);
            dealloc(pointer, layout);
        }
        assert_eq!(snapshot().map_calls, maps);
        assert_eq!(snapshot().current_bytes, 0);
    }
}

pub(crate) fn memory_stats() -> serde_json::Value {
    let value = snapshot();
    serde_json::json!({"enabled":value.enabled,"thresholdBytes":value.threshold_bytes,
        "currentBytes":value.current_bytes,"peakBytes":value.peak_bytes,
        "mapCalls":value.map_calls,"unmapCalls":value.unmap_calls,
        "mapFailures":value.map_failures,"unmapFailures":value.unmap_failures})
}
