//! Holding sleep for the manager's hand-over (M58 step 3; design section 4, "Going to sleep
//! without pressing the switch"). GPUI's sleep notice is queued and does not hold sleep, so on
//! macOS this registers with I/O Kit (`IORegisterForSystemPower`, a system framework; no crate)
//! and delays `IOAllowPowerChange` until the hand-over ends, or `limit` passes (at most 20 s,
//! proof P17). Idle sleep that asks first is allowed at once; nothing here ever refuses sleep.
//!
//! [`handle`] holds the rules, with the allow and the hand-over passed in, so they are tested with
//! fakes: no test holds or starts the Mac's sleep. Other systems: [`hold_sleep`] does nothing.

use std::sync::Arc;
use std::time::Duration;

/// `kIOMessageCanSystemSleep`: idle sleep asks first.
pub const CAN_SLEEP: u32 = 0xE000_0270;
/// `kIOMessageSystemWillSleep`: sleep is coming (forced, a closed lid, or idle once allowed).
pub const WILL_SLEEP: u32 = 0xE000_0280;
/// `kIOMessageSystemHasPoweredOn`: woken.
pub const POWERED_ON: u32 = 0xE000_0300;

/// What runs at sleep and at wake (on a thread of their own; the hand-over may block).
pub struct Handlers {
    pub on_sleep: Box<dyn Fn() + Send + Sync>,
    pub on_wake: Box<dyn Fn() + Send + Sync>,
}

/// One power message: sleep is allowed (`allow`) after the hand-over returns, or after `limit`,
/// whichever comes first; idle sleep that asks is allowed at once; a wake runs `on_wake`. Every
/// sleep message is answered, always.
pub fn handle(message: u32, handlers: &Arc<Handlers>, limit: Duration, allow: &dyn Fn()) {
    match message {
        CAN_SLEEP => allow(),
        WILL_SLEEP => {
            let (done, wait) = std::sync::mpsc::channel::<()>();
            let handlers = Arc::clone(handlers);
            let spawned = std::thread::Builder::new().name("glasscode-sleep-handover".into()).spawn(move || {
                (handlers.on_sleep)();
                let _ = done.send(());
            });
            if spawned.is_ok() {
                // Done, or the limit: either way sleep goes on.
                let _ = wait.recv_timeout(limit);
            }
            allow();
        }
        POWERED_ON => {
            let handlers = Arc::clone(handlers);
            let _ = std::thread::Builder::new().name("glasscode-wake".into()).spawn(move || (handlers.on_wake)());
        }
        _ => {}
    }
}

/// Register for the system's power messages, holding sleep up to `limit` for `handlers` (macOS).
/// Returns whether it is registered. Elsewhere it does nothing and returns false.
#[cfg(target_os = "macos")]
pub fn hold_sleep(limit: Duration, handlers: Handlers) -> bool {
    mac::register(limit, Arc::new(handlers))
}

#[cfg(not(target_os = "macos"))]
pub fn hold_sleep(_limit: Duration, _handlers: Handlers) -> bool {
    false
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    type IoConnect = u32;
    type IoObject = u32;
    type NotificationPort = *mut c_void;
    type RunLoop = *mut c_void;
    type RunLoopSource = *mut c_void;
    type CfString = *const c_void;
    type Callback = extern "C" fn(refcon: *mut c_void, service: IoObject, message: u32, argument: *mut c_void);

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IORegisterForSystemPower(refcon: *mut c_void, port: *mut NotificationPort, callback: Callback, notifier: *mut IoObject) -> IoConnect;
        fn IONotificationPortGetRunLoopSource(port: NotificationPort) -> RunLoopSource;
        fn IOAllowPowerChange(kernel_port: IoConnect, notification: isize) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopGetCurrent() -> RunLoop;
        fn CFRunLoopAddSource(run_loop: RunLoop, source: RunLoopSource, mode: CfString);
        fn CFRunLoopRun();
        static kCFRunLoopDefaultMode: CfString;
    }

    struct Registered {
        root: IoConnect,
        limit: Duration,
        handlers: Arc<super::Handlers>,
    }

    static REGISTERED: OnceLock<Registered> = OnceLock::new();

    extern "C" fn on_message(_refcon: *mut c_void, _service: IoObject, message: u32, argument: *mut c_void) {
        let Some(registered) = REGISTERED.get() else { return };
        let notification = argument as isize;
        let allow = || {
            // SAFETY: the root port I/O Kit gave at registration, and the message's own id.
            unsafe { IOAllowPowerChange(registered.root, notification) };
        };
        super::handle(message, &registered.handlers, registered.limit, &allow);
    }

    /// Once per process: a thread with its own run loop receives the messages.
    pub fn register(limit: Duration, handlers: Arc<super::Handlers>) -> bool {
        if REGISTERED.get().is_some() {
            return false;
        }
        let (ready, wait) = std::sync::mpsc::channel::<bool>();
        let spawned = std::thread::Builder::new().name("glasscode-power".into()).spawn(move || {
            let mut port: NotificationPort = std::ptr::null_mut();
            let mut notifier: IoObject = 0;
            // SAFETY: out-pointers to locals; the callback is a plain extern fn.
            let root = unsafe { IORegisterForSystemPower(std::ptr::null_mut(), &mut port, on_message, &mut notifier) };
            if root == 0 || port.is_null() || REGISTERED.set(Registered { root, limit, handlers }).is_err() {
                let _ = ready.send(false);
                return;
            }
            // SAFETY: the port's source goes on this thread's run loop, which runs for the process.
            unsafe {
                CFRunLoopAddSource(CFRunLoopGetCurrent(), IONotificationPortGetRunLoopSource(port), kCFRunLoopDefaultMode);
            }
            let _ = ready.send(true);
            unsafe { CFRunLoopRun() };
        });
        spawned.is_ok() && wait.recv_timeout(Duration::from_secs(5)).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    use super::*;

    fn handlers(sleep_for: Duration, slept: Arc<AtomicU32>, woke: Arc<AtomicU32>) -> Arc<Handlers> {
        Arc::new(Handlers {
            on_sleep: Box::new(move || {
                std::thread::sleep(sleep_for);
                slept.fetch_add(1, Ordering::SeqCst);
            }),
            on_wake: Box::new(move || {
                woke.fetch_add(1, Ordering::SeqCst);
            }),
        })
    }

    /// A fake allow: when it was called, after the message came.
    fn allow_into(at: &Mutex<Option<Duration>>, start: Instant) -> impl Fn() + '_ {
        move || *at.lock().unwrap() = Some(start.elapsed())
    }

    #[test]
    fn sleep_waits_for_the_hand_over_then_goes_on() {
        let (slept, woke) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
        let handlers = handlers(Duration::from_millis(30), slept.clone(), woke.clone());
        let at = Mutex::new(None);
        handle(WILL_SLEEP, &handlers, Duration::from_secs(5), &allow_into(&at, Instant::now()));
        assert_eq!(slept.load(Ordering::SeqCst), 1, "the hand-over ran before sleep was allowed");
        assert!(at.lock().unwrap().is_some(), "sleep is always allowed");
    }

    #[test]
    fn a_slow_hand_over_never_holds_sleep_past_the_limit() {
        let (slept, woke) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
        let handlers = handlers(Duration::from_millis(500), slept, woke);
        let at = Mutex::new(None);
        handle(WILL_SLEEP, &handlers, Duration::from_millis(50), &allow_into(&at, Instant::now()));
        let waited = at.lock().unwrap().expect("allowed");
        assert!(waited < Duration::from_millis(400), "allowed at the limit, not after the hand-over: {waited:?}");
    }

    #[test]
    fn idle_sleep_is_allowed_at_once_and_a_wake_runs_its_handler() {
        let (slept, woke) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
        let handlers = handlers(Duration::from_millis(500), slept.clone(), woke.clone());
        let at = Mutex::new(None);
        handle(CAN_SLEEP, &handlers, Duration::from_secs(5), &allow_into(&at, Instant::now()));
        assert!(at.lock().unwrap().is_some_and(|d| d < Duration::from_millis(100)));
        assert_eq!(slept.load(Ordering::SeqCst), 0, "asking is not sleeping: no hand-over");
        handle(POWERED_ON, &handlers, Duration::from_secs(5), &|| panic!("a wake allows nothing"));
        let start = Instant::now();
        while woke.load(Ordering::SeqCst) == 0 && start.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(woke.load(Ordering::SeqCst), 1);
    }
}
