//! Process-lifetime memory hygiene for the embedded AppKit loop.
//!
//! Bun's main thread has no autorelease pool of its own. Everything AppKit,
//! CoreText or Foundation autoreleases for work that runs outside
//! `pump_events` (napi calls from React commits, `bun:ffi` reads in JS timers)
//! lands in the implicit outermost pool, which never drains. `tick_pool`
//! keeps one pool open between ticks and drains it at the start of the next
//! one. `KYBERN_TICK_POOL=0` restores the old behaviour for comparisons.
//!
//! `set_window_state_callback` tells JS when AppKit reports an occlusion,
//! key, move, screen, appearance or accessibility-display change, so the JS
//! side reads window state only when it changed instead of polling.
#[cfg(target_os = "macos")]
mod pool {
    use std::{cell::Cell, ffi::c_void, sync::OnceLock};
    unsafe extern "C" {
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }
    thread_local! {
        static OPEN: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
    }
    static CYCLES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var("KYBERN_TICK_POOL").as_deref() != Ok("0"))
    }
    /// Drain what was autoreleased since the previous tick, then open the pool
    /// this tick and the JS work after it run inside. Pools nest: AppKit's own
    /// pools (`pump_events`, event dispatch) push and pop above this one.
    pub(crate) fn cycle() {
        if !enabled() {
            return;
        }
        OPEN.with(|open| unsafe {
            let previous = open.replace(std::ptr::null_mut());
            if !previous.is_null() {
                objc_autoreleasePoolPop(previous);
            }
            open.set(objc_autoreleasePoolPush());
        });
        CYCLES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    pub(crate) fn stats() -> serde_json::Value {
        serde_json::json!({ "enabled": enabled(), "cycles": CYCLES.load(std::sync::atomic::Ordering::Relaxed) })
    }
}

pub(crate) fn tick_pool() {
    #[cfg(target_os = "macos")]
    pool::cycle();
}

pub(crate) fn tick_pool_stats() -> serde_json::Value {
    #[cfg(target_os = "macos")]
    return pool::stats();
    #[cfg(not(target_os = "macos"))]
    serde_json::Value::Null
}

/// One queued JS delivery at a time: AppKit can post several notifications for
/// one user action (minimize = occlusion + key + move); JS re-reads all state.
#[cfg(target_os = "macos")]
static STATE_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn acknowledge_window_state() {
    #[cfg(target_os = "macos")]
    STATE_PENDING.store(false, std::sync::atomic::Ordering::Release);
}

pub(crate) fn set_window_state_callback(
    callback: Option<napi::threadsafe_function::ThreadsafeFunction<serde_json::Value>>,
) {
    #[cfg(target_os = "macos")]
    {
        use napi::threadsafe_function::ThreadsafeFunctionCallMode;
        acknowledge_window_state();
        gpui_macos::set_window_state_observer(callback.map(|callback| {
            std::sync::Arc::new(move || {
                if !STATE_PENDING.swap(true, std::sync::atomic::Ordering::AcqRel)
                    && callback.call(Ok(serde_json::Value::Null), ThreadsafeFunctionCallMode::NonBlocking) != napi::Status::Ok
                {
                    acknowledge_window_state();
                }
            }) as std::sync::Arc<dyn Fn() + Send + Sync>
        }));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = callback;
}
