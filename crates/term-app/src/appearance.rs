use std::ffi::OsString;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::time::Duration;
use winit::window::Theme;

#[derive(Default)]
struct Stop {
    stopped: bool,
    waker: Option<Waker>,
}

pub struct Watch {
    stop: Arc<Mutex<Stop>>,
    thread: Option<std::thread::JoinHandle<()>>,
    initial: Option<mpsc::Receiver<Option<Theme>>>,
}

impl Watch {
    /// Waits up to `timeout` for the portal's first answer; `None` if it has
    /// none yet (it still arrives through the callback) or was already taken.
    pub fn initial(&mut self, timeout: Duration) -> Option<Option<Theme>> {
        self.initial.take()?.recv_timeout(timeout).ok()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let mut stop = self.stop.lock().unwrap();
        stop.stopped = true;
        if let Some(waker) = stop.waker.take() {
            waker.wake();
        }
        drop(stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn spawn(
    future: impl Future<Output = ()> + Send + 'static,
    initial: Option<mpsc::Receiver<Option<Theme>>>,
) -> std::io::Result<Watch> {
    let stop = Arc::new(Mutex::new(Stop::default()));
    let cancelled = stop.clone();
    let thread = std::thread::Builder::new()
        .name("appearance-watch".into())
        .spawn(move || {
            zbus::block_on(futures_lite::future::race(
                std::future::poll_fn(move |cx| {
                    let mut stop = cancelled.lock().unwrap();
                    if stop.stopped {
                        Poll::Ready(())
                    } else {
                        stop.waker = Some(cx.waker().clone());
                        Poll::Pending
                    }
                }),
                future,
            ));
        })?;
    Ok(Watch {
        stop,
        thread: Some(thread),
        initial,
    })
}

/// Without either variable zbus falls back to `/run/user/$UID/bus`, which
/// outside a login session can be a bus whose portal never answers.
fn session_bus_configured(address: Option<OsString>, runtime_dir: Option<OsString>) -> bool {
    address.is_some() || runtime_dir.is_some()
}

pub fn watch(changed: impl Fn(Option<Theme>) + Send + 'static) -> std::io::Result<Watch> {
    if !session_bus_configured(
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS"),
        std::env::var_os("XDG_RUNTIME_DIR"),
    ) {
        diag!(
            "terminal: OS appearance unavailable (using dark): DBUS_SESSION_BUS_ADDRESS and XDG_RUNTIME_DIR are unset"
        );
        return Ok(Watch {
            stop: Arc::default(),
            thread: None,
            initial: None,
        });
    }
    let (first, initial) = mpsc::sync_channel(1);
    spawn(
        async move {
            if let Err(error) = listen(first, changed).await {
                diag!("terminal: OS appearance unavailable (using dark): {error}");
            }
        },
        Some(initial),
    )
}

const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "color-scheme";

fn appearance(value: &zbus::Variant<'_>) -> Option<Theme> {
    match value.downcast_ref::<u32>().ok()? {
        1 => Some(Theme::Dark),
        2 => Some(Theme::Light),
        _ => None,
    }
}

async fn listen(
    first: mpsc::SyncSender<Option<Theme>>,
    changed: impl Fn(Option<Theme>),
) -> zbus::Result<()> {
    use futures_lite::StreamExt;
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
    )
    .await?;
    // Subscribe before reading so a change during startup cannot be lost.
    let mut signals = proxy.receive_signal("SettingChanged").await?;
    let value: zbus::OwnedVariant = match proxy.call("ReadOne", &(NAMESPACE, KEY)).await {
        Ok(value) => value,
        Err(_) => proxy.call("Read", &(NAMESPACE, KEY)).await?,
    };
    let _ = first.try_send(appearance(&value));
    drop(first);
    changed(appearance(&value));
    while let Some(signal) = signals.next().await {
        if let Ok((namespace, key, value)) = signal
            .body()
            .deserialize::<(String, String, zbus::OwnedVariant)>()
            && namespace == NAMESPACE
            && key == KEY
        {
            changed(appearance(&value));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_finished_watch() {
        let (done, finished) = std::sync::mpsc::channel();
        let watch = spawn(async move { done.send(()).unwrap() }, None).unwrap();
        finished
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        drop(watch);
    }

    #[test]
    fn dropping_watch_cancels_pending_work_and_allows_restart() {
        for _ in 0..2 {
            let (ready, started) = std::sync::mpsc::channel();
            let (dropped, finished) = std::sync::mpsc::channel();
            struct OnDrop(std::sync::mpsc::Sender<()>);
            impl Drop for OnDrop {
                fn drop(&mut self) {
                    let _ = self.0.send(());
                }
            }
            let watch = spawn(
                async move {
                    let _guard = OnDrop(dropped);
                    ready.send(()).unwrap();
                    std::future::pending::<()>().await;
                },
                None,
            )
            .unwrap();
            started
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            drop(watch);
            finished.try_recv().unwrap();
        }
    }

    #[test]
    fn session_bus_needs_an_address_or_runtime_dir() {
        let set = || Some(OsString::from("x"));
        assert!(session_bus_configured(set(), set()));
        assert!(session_bus_configured(set(), None));
        assert!(session_bus_configured(None, set()));
        assert!(!session_bus_configured(None, None));
    }

    #[test]
    fn portal_color_scheme_values() {
        for (value, expected) in [
            (0, None),
            (1, Some(Theme::Dark)),
            (2, Some(Theme::Light)),
            (3, None),
        ] {
            assert_eq!(appearance(&zbus::Variant::U32(value)), expected);
        }
        assert_eq!(appearance(&zbus::Variant::from("invalid")), None);
        assert_eq!(
            appearance(&zbus::Variant::Variant(Box::new(zbus::Variant::U32(2)))),
            Some(Theme::Light)
        );
    }
}
