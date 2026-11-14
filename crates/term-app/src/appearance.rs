use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use winit::window::Theme;

#[derive(Default)]
struct Stop {
    stopped: bool,
    waker: Option<Waker>,
}

pub struct Watch {
    stop: Arc<Mutex<Stop>>,
    thread: Option<std::thread::JoinHandle<()>>,
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

fn spawn(future: impl Future<Output = ()> + Send + 'static) -> std::io::Result<Watch> {
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
    })
}

pub fn watch(changed: impl Fn(Option<Theme>) + Send + 'static) -> std::io::Result<Watch> {
    spawn(async move {
        if let Err(error) = listen(changed).await {
            diag!("terminal: OS appearance unavailable (using dark): {error}");
        }
    })
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

async fn listen(changed: impl Fn(Option<Theme>)) -> zbus::Result<()> {
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
        let watch = spawn(async move { done.send(()).unwrap() }).unwrap();
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
            let watch = spawn(async move {
                let _guard = OnDrop(dropped);
                ready.send(()).unwrap();
                std::future::pending::<()>().await;
            })
            .unwrap();
            started
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            drop(watch);
            finished.try_recv().unwrap();
        }
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
