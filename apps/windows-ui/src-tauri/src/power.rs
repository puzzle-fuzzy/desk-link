use std::{
    ffi::c_void,
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tauri::AppHandle;
use windows::Win32::{
    Foundation::HANDLE,
    System::Power::{
        DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY, PowerRegisterSuspendResumeNotification,
        PowerUnregisterSuspendResumeNotification,
    },
    UI::WindowsAndMessaging::{
        DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMECRITICAL, PBT_APMRESUMESUSPEND,
    },
};

use crate::host::HostManager;
use apps_windows::diagnostics::{DiagnosticEvent, DiagnosticLog};

const RESUME_DEBOUNCE: Duration = Duration::from_secs(3);

type ResumeAction = Arc<dyn Fn() + Send + Sync + 'static>;

struct ResumeDebouncer {
    last_restart: Option<Instant>,
}

impl ResumeDebouncer {
    const fn new() -> Self {
        Self { last_restart: None }
    }

    fn accept(&mut self, now: Instant) -> bool {
        if self
            .last_restart
            .is_some_and(|last| now.saturating_duration_since(last) < RESUME_DEBOUNCE)
        {
            return false;
        }
        self.last_restart = Some(now);
        true
    }
}

struct PowerResumeContext {
    subscription: DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS,
    on_resume: ResumeAction,
    debouncer: Mutex<ResumeDebouncer>,
}

pub struct PowerResumeMonitor {
    registration: HPOWERNOTIFY,
    context: Option<Box<PowerResumeContext>>,
}

// The callback context is pinned by its Box, and all state reached by the Windows callback is
// either immutable or protected by a Mutex. Unregistration completes before the context is freed.
unsafe impl Send for PowerResumeMonitor {}
unsafe impl Sync for PowerResumeMonitor {}

impl Drop for PowerResumeMonitor {
    fn drop(&mut self) {
        let Some(context) = self.context.take() else {
            return;
        };
        let result = unsafe { PowerUnregisterSuspendResumeNotification(self.registration) };
        if result.0 != 0 {
            // The callback may still reference this allocation when Windows cannot unregister it.
            // Leaking at process shutdown is safer than freeing callback state prematurely.
            Box::leak(context);
        }
    }
}

pub fn install(app: &AppHandle, manager: HostManager) -> io::Result<PowerResumeMonitor> {
    let resume_app = app.clone();
    let on_resume: ResumeAction = Arc::new(move || {
        if let Ok(diagnostics) = DiagnosticLog::for_current_user() {
            let _ = diagnostics.record(&DiagnosticEvent::PowerResumeDetected);
        }
        let manager = manager.clone();
        let app = resume_app.clone();
        tauri::async_runtime::spawn(async move {
            manager.restart(app).await;
        });
    });
    let monitor = register(on_resume)?;
    if let Ok(diagnostics) = DiagnosticLog::for_current_user() {
        let _ = diagnostics.record(&DiagnosticEvent::PowerResumeMonitoringStarted);
    }
    Ok(monitor)
}

fn register(on_resume: ResumeAction) -> io::Result<PowerResumeMonitor> {
    let mut context = Box::new(PowerResumeContext {
        subscription: DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(power_notification_callback),
            Context: std::ptr::null_mut(),
        },
        on_resume,
        debouncer: Mutex::new(ResumeDebouncer::new()),
    });
    context.subscription.Context = (&mut *context as *mut PowerResumeContext).cast();

    let recipient = HANDLE(
        (&context.subscription as *const DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS)
            .cast_mut()
            .cast::<c_void>(),
    );
    let mut registration = std::ptr::null_mut();
    let result = unsafe {
        PowerRegisterSuspendResumeNotification(DEVICE_NOTIFY_CALLBACK, recipient, &mut registration)
    };
    if result.0 != 0 {
        return Err(io::Error::from_raw_os_error(result.0 as i32));
    }
    if registration.is_null() {
        return Err(io::Error::other(
            "Windows returned an empty power notification registration",
        ));
    }

    Ok(PowerResumeMonitor {
        registration: HPOWERNOTIFY(registration as isize),
        context: Some(context),
    })
}

unsafe extern "system" fn power_notification_callback(
    context: *const c_void,
    event_type: u32,
    _setting: *const c_void,
) -> u32 {
    if context.is_null() || !is_resume_event(event_type) {
        return 0;
    }
    let context = unsafe { &*(context.cast::<PowerResumeContext>()) };
    if context
        .debouncer
        .lock()
        .is_ok_and(|mut debouncer| debouncer.accept(Instant::now()))
    {
        (context.on_resume)();
    }
    0
}

fn is_resume_event(power_event: u32) -> bool {
    matches!(
        power_event,
        PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMECRITICAL | PBT_APMRESUMESUSPEND
    )
}

#[cfg(test)]
mod tests {
    use super::{
        PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PowerResumeContext, RESUME_DEBOUNCE,
        ResumeDebouncer, power_notification_callback, register,
    };
    use std::{
        ptr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    #[test]
    fn only_resume_notifications_restart_hosting() {
        let restarts = Arc::new(AtomicUsize::new(0));
        let observed = restarts.clone();
        let monitor = register(Arc::new(move || {
            observed.fetch_add(1, Ordering::Relaxed);
        }))
        .unwrap();
        let context: *const PowerResumeContext = monitor.context.as_ref().unwrap().as_ref();

        unsafe {
            power_notification_callback(context.cast(), 4, ptr::null());
            power_notification_callback(context.cast(), PBT_APMRESUMEAUTOMATIC, ptr::null());
        }
        assert_eq!(restarts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn duplicate_resume_notifications_are_debounced() {
        let start = Instant::now();
        let mut debouncer = ResumeDebouncer::new();
        assert!(debouncer.accept(start));
        assert!(!debouncer.accept(start + RESUME_DEBOUNCE - Duration::from_millis(1)));
        assert!(debouncer.accept(start + RESUME_DEBOUNCE));
    }

    #[test]
    fn windows_accepts_suspend_resume_callback_registration() {
        let monitor = register(Arc::new(|| {})).unwrap();
        assert!(!monitor.registration.is_invalid());
        assert!(monitor.context.is_some());
        assert_eq!(PBT_APMRESUMESUSPEND, 7);
    }
}
