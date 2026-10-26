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
    // Off macOS the window's own observers deliver (`observe_window_state`, installed by the renderer).
    #[cfg(not(target_os = "macos"))]
    let _ = callback;
}

/// What off-macOS window-state deliveries carry, so JS needs no read of its own: `minimized` and
/// `visible` (not minimized, shown and not cloaked on another virtual desktop) on Windows, null where
/// the platform cannot tell (Linux); `focused` is GPUI's window activation.
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd"))]
fn window_state_report(window: &gpui::Window) -> serde_json::Value {
    #[cfg(target_os = "windows")]
    let (minimized, visible) = {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows::Win32::{Foundation::HWND, Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED}, UI::WindowsAndMessaging::{IsIconic, IsWindowVisible}};
        match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => unsafe {
                let hwnd = HWND(handle.hwnd.get() as *mut std::ffi::c_void);
                let minimized = IsIconic(hwnd).as_bool();
                let mut cloaked = 0u32;
                let cloaked = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4).is_ok() && cloaked != 0;
                (Some(minimized), Some(!minimized && IsWindowVisible(hwnd).as_bool() && !cloaked))
            },
            _ => (None, None),
        }
    };
    #[cfg(not(target_os = "windows"))]
    let (minimized, visible): (Option<bool>, Option<bool>) = (None, None);
    serde_json::json!({ "minimized": minimized, "visible": visible, "focused": window.is_window_active() })
}

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd"))]
thread_local! {
    static STATE_OBSERVERS: std::cell::RefCell<Vec<gpui::Subscription>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Off macOS: call `callback` with `window_state_report` when it changes, and on every appearance change (JS re-reads
/// the theme and accessibility display then). GPUI's bounds notification covers minimize and restore (Windows moves a
/// minimized window to -32000 and resizes it back); activation covers focus. A report is checked again shortly after
/// each notification, since a minimize can notify before the window reports iconic. `None` stops.
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd"))]
pub(crate) fn observe_window_state(
    callback: Option<napi::threadsafe_function::ThreadsafeFunction<serde_json::Value>>,
    window: &mut gpui::Window,
    cx: &mut gpui::Context<crate::renderer::GpuixView>,
) {
    use napi::threadsafe_function::ThreadsafeFunctionCallMode;
    use std::{cell::RefCell, rc::Rc};
    let Some(callback) = callback else {
        STATE_OBSERVERS.with(|observers| observers.borrow_mut().clear());
        return;
    };
    let callback = std::sync::Arc::new(callback);
    let last = Rc::new(RefCell::new(serde_json::Value::Null));
    let send = {
        let (callback, last) = (callback.clone(), last.clone());
        move |window: &gpui::Window, always: bool| {
            let report = window_state_report(window);
            if !always && *last.borrow() == report {
                return;
            }
            *last.borrow_mut() = report.clone();
            callback.call(Ok(report), ThreadsafeFunctionCallMode::NonBlocking);
        }
    };
    let send = Rc::new(send);
    let recheck = Rc::new(std::cell::Cell::new(false));
    let notify = move |always: bool| {
        let (send, recheck) = (send.clone(), recheck.clone());
        move |_: &mut crate::renderer::GpuixView, window: &mut gpui::Window, cx: &mut gpui::Context<crate::renderer::GpuixView>| {
            send(window, always);
            if recheck.replace(true) {
                return;
            }
            let (send, recheck) = (send.clone(), recheck.clone());
            cx.spawn_in(window, async move |view, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                recheck.set(false);
                view.update_in(cx, |_, window, _| send(window, false)).ok();
            })
            .detach();
        }
    };
    let subscriptions = vec![
        cx.observe_window_bounds(window, notify(false)),
        cx.observe_window_activation(window, notify(false)),
        cx.observe_window_appearance(window, notify(true)),
    ];
    STATE_OBSERVERS.with(|observers| *observers.borrow_mut() = subscriptions);
    let report = window_state_report(window);
    *last.borrow_mut() = report.clone();
    callback.call(Ok(report), ThreadsafeFunctionCallMode::NonBlocking);
}
