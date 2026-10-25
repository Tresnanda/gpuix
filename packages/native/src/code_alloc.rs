// Included by large_alloc.rs. Code layout uses many 1 MiB arena chunks and
// intermediate tables below the pixel allocator's original 4 MiB boundary.
// Darwin's shared small-zone pages stay charged after those allocations die.
// Keep these medium buffers on the existing scoped mmap/munmap route too.
fn allocation_threshold() -> usize {
    #[cfg(target_os = "macos")]
    {
        use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
        static CONFIG: AtomicU8 = AtomicU8::new(0);
        let mut config = CONFIG.load(Relaxed);
        if config == 0 {
            unsafe extern "C" { fn getenv(name: *const std::ffi::c_char) -> *const std::ffi::c_char; }
            let value = unsafe { getenv(c"KYBERN_CODE_MEMORY".as_ptr()) };
            config = if !value.is_null() && unsafe { *value } == b'0' as std::ffi::c_char { 2 } else { 1 };
            CONFIG.store(config, Relaxed);
        }
        if config == 1 { 64 * 1024 } else { THRESHOLD }
    }
    #[cfg(not(target_os = "macos"))]
    { THRESHOLD }
}

#[cfg(all(test, target_os = "macos"))]
mod code_buffer_tests {
    use super::*;
    use std::alloc::{alloc_zeroed, dealloc, realloc};

    #[test]
    fn medium_buffers_cross_the_effective_boundary_and_release() {
        let threshold = allocation_threshold();
        assert_eq!(snapshot().threshold_bytes, threshold);
        let expected = if std::env::var("KYBERN_CODE_MEMORY").as_deref() == Ok("0") { THRESHOLD } else { 64 * 1024 };
        assert_eq!(threshold, expected);
        let before = snapshot().current_bytes;
        let small = Layout::from_size_align(threshold - 16, 64).unwrap();
        unsafe {
            let pointer = alloc_zeroed(small);
            assert!(!pointer.is_null());
            *pointer = 41;
            *pointer.add(small.size() - 1) = 73;
            assert_eq!(snapshot().current_bytes, before);
            let size = threshold + 128;
            let mapped = realloc(pointer, small, size);
            assert!(!mapped.is_null());
            assert_eq!(mapped as usize % 64, 0);
            assert_eq!(*mapped, 41);
            assert_eq!(*mapped.add(small.size() - 1), 73);
            assert_eq!(snapshot().current_bytes, before + size);
            let small_again = realloc(mapped, Layout::from_size_align(size, 64).unwrap(), 1024);
            assert!(!small_again.is_null());
            assert_eq!(*small_again, 41);
            assert_eq!(snapshot().current_bytes, before);
            dealloc(small_again, Layout::from_size_align(1024, 64).unwrap());
        }
        assert_eq!(snapshot().current_bytes, before);
    }
}
