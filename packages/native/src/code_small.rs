// Private Rust small blocks; never override libc, AppKit, JSC or other modules.
#[cfg(target_os = "macos")]
mod code_small {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering::Relaxed};
    static CONFIG: AtomicU8 = AtomicU8::new(0);
    static FREED: AtomicUsize = AtomicUsize::new(0);
    static COLLECTIONS: AtomicUsize = AtomicUsize::new(0);
    static FRAME_COLLECTIONS: AtomicUsize = AtomicUsize::new(0);
    static TICK_REQUEST: AtomicBool = AtomicBool::new(false);
    static FRAME_REQUEST: AtomicBool = AtomicBool::new(false);
    const COLLECT_AFTER_BYTES: usize = 8 * 1024 * 1024;
    pub(super) fn enabled() -> bool {
        let mut config = CONFIG.load(Relaxed);
        if config == 0 {
            unsafe extern "C" { fn getenv(name: *const std::ffi::c_char) -> *const std::ffi::c_char; }
            let mut enabled = true;
            for name in [c"KYBERN_CODE_MEMORY", c"KYBERN_CODE_SMALL_MEMORY", c"KYBERN_LARGE_ALLOCATIONS_VM"] {
                let value = unsafe { getenv(name.as_ptr()) };
                enabled &= value.is_null() || unsafe { *value } != b'0' as std::ffi::c_char;
            }
            config = if enabled { 1 } else { 2 };
            CONFIG.store(config, Relaxed);
        }
        config == 1
    }
    pub(super) struct Small;
    unsafe impl GlobalAlloc for Small {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if enabled() { unsafe { mimalloc::MiMalloc.alloc(layout) } } else { unsafe { System.alloc(layout) } }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            if enabled() { unsafe { mimalloc::MiMalloc.alloc_zeroed(layout) } } else { unsafe { System.alloc_zeroed(layout) } }
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            if enabled() { unsafe { mimalloc::MiMalloc.dealloc(pointer, layout) } } else { unsafe { System.dealloc(pointer, layout) } }
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            if enabled() { unsafe { mimalloc::MiMalloc.realloc(pointer, layout, size) } } else { unsafe { System.realloc(pointer, layout, size) } }
        }
    }
    pub(super) fn released_mapping(bytes: usize) {
        if enabled() { FREED.fetch_add(bytes, Relaxed); }
    }
    fn post_enabled() -> bool {
        static CONFIG: AtomicU8 = AtomicU8::new(0);
        let mut config = CONFIG.load(Relaxed);
        if config == 0 {
            unsafe extern "C" { fn getenv(name: *const std::ffi::c_char) -> *const std::ffi::c_char; }
            let value = unsafe { getenv(c"KYBERN_CODE_POST_FRAME_MEMORY".as_ptr()) };
            config = if !value.is_null() && unsafe { *value } == b'0' as std::ffi::c_char { 2 } else { 1 };
            CONFIG.store(config, Relaxed);
        }
        config == 1
    }
    pub(super) fn request() {
        if enabled() && post_enabled() {
            TICK_REQUEST.store(true, Relaxed);
            FRAME_REQUEST.store(true, Relaxed);
        }
    }
    fn collect() {
        FREED.swap(0, Relaxed);
        unsafe { libmimalloc_sys::mi_collect(true); }
        COLLECTIONS.fetch_add(1, Relaxed);
    }
    // After the existing AppKit pump returns, frame/arena destructors have run.
    // No new frame, timer, or collection from inside allocator callbacks.
    pub(super) fn collect_if_needed() {
        if enabled() {
            let requested = TICK_REQUEST.swap(false, Relaxed);
            if requested || FREED.load(Relaxed) >= COLLECT_AFTER_BYTES { collect(); }
        }
    }
    pub(super) fn after_frame(contracted: bool) {
        if enabled() && post_enabled() {
            let requested = FRAME_REQUEST.swap(false, Relaxed);
            if requested || contracted || FREED.load(Relaxed) >= COLLECT_AFTER_BYTES {
                TICK_REQUEST.store(false, Relaxed);
                collect();
                FRAME_COLLECTIONS.fetch_add(1, Relaxed);
            }
        }
    }
    pub(super) fn stats() -> serde_json::Value {
        serde_json::json!({"enabled":enabled(),"postFrameEnabled":post_enabled(),"collections":COLLECTIONS.load(Relaxed),"frameCollections":FRAME_COLLECTIONS.load(Relaxed),"pendingFreedBytes":FREED.load(Relaxed),"frameRequested":FRAME_REQUEST.load(Relaxed),"tickRequested":TICK_REQUEST.load(Relaxed)})
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn small_realloc_preserves_alignment_and_bytes() {
            for align in [1, 16, 64, 4096, 65536] {
                let layout = Layout::from_size_align(137, align).unwrap();
                unsafe {
                    let p = Small.alloc_zeroed(layout);
                    assert!(!p.is_null()); assert_eq!(p as usize % align, 0);
                    assert_eq!(*p.add(136), 0); *p = 53; *p.add(136) = 91;
                    let q = Small.realloc(p, layout, 4097);
                    assert!(!q.is_null()); assert_eq!(q as usize % align, 0);
                    assert_eq!(*q, 53); assert_eq!(*q.add(136), 91);
                    Small.dealloc(q, Layout::from_size_align(4097, align).unwrap());
                }
            }
        }
        #[test]
        fn blocks_can_be_released_by_another_thread() {
            let data = vec![37u8; 8192];
            std::thread::spawn(move || { assert_eq!(data[8191], 37); drop(data); }).join().unwrap();
        }
        #[test]
        fn collection_is_event_driven_and_coalesced() {
            if !enabled() { return; }
            collect_if_needed(); FREED.store(0, Relaxed);
            let before = COLLECTIONS.load(Relaxed);
            released_mapping(COLLECT_AFTER_BYTES - 1); collect_if_needed();
            assert_eq!(COLLECTIONS.load(Relaxed), before);
            released_mapping(1); collect_if_needed();
            assert_eq!(COLLECTIONS.load(Relaxed), before + 1);
            collect_if_needed();
            assert_eq!(COLLECTIONS.load(Relaxed), before + 1);
        }
        #[test]
        fn release_requests_survive_an_early_pump_and_coalesce_after_a_frame() {
            if !enabled() || !post_enabled() { return; }
            collect_if_needed();after_frame(false);FREED.store(0, Relaxed);
            let before=COLLECTIONS.load(Relaxed);
            request();request();collect_if_needed();
            assert_eq!(COLLECTIONS.load(Relaxed),before+1);
            after_frame(false);after_frame(false);collect_if_needed();
            assert_eq!(COLLECTIONS.load(Relaxed),before+2);
            request();after_frame(false);collect_if_needed();
            assert_eq!(COLLECTIONS.load(Relaxed),before+3);
        }
    }
}
