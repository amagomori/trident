use winit::window::Theme;

pub fn watch(changed: impl Fn(Option<Theme>) + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("appearance-watch".into())
        .spawn(move || {
            if let Err(error) = zbus::block_on(listen(changed)) {
                eprintln!("terminal: OS appearance unavailable (using dark): {error}");
            }
        })
        .map(|_| ())
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
