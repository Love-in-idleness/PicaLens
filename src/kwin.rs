use std::time::Duration;

use ashpd::zbus::{self, Proxy, fdo::DBusProxy};
use futures_util::{FutureExt, StreamExt};

use crate::session::{Result, Stop};

const SERVICE: &str = "org.kde.KWin";
const EFFECT: &str = "picalens";

pub async fn discover() -> Result<Option<Proxy<'static>>> {
    let bus = zbus::connection::Builder::session()?
        .method_timeout(Duration::from_secs(3))
        .build()
        .await?;
    if !DBusProxy::new(&bus)
        .await?
        .name_has_owner(SERVICE.try_into()?)
        .await?
    {
        return Ok(None);
    }
    let effects = Proxy::new(&bus, SERVICE, "/Effects", "org.kde.kwin.Effects").await?;
    let loaded: bool = effects.call("isEffectLoaded", &(EFFECT,)).await?;
    if !loaded {
        let supported: bool = effects.call("isEffectSupported", &(EFFECT,)).await?;
        if !supported {
            return Ok(None);
        }
        let loaded: bool = effects.call("loadEffect", &(EFFECT,)).await?;
        if !loaded {
            return Err("KWin could not load the PicaLens backend".into());
        }
    }
    Ok(Some(
        Proxy::new(
            &bus,
            SERVICE,
            "/io/github/LoveInIdleness/PicaLens/Backend",
            "io.github.LoveInIdleness.PicaLens.Backend1",
        )
        .await?,
    ))
}

pub async fn magnify(backend: Proxy<'_>, mut stop: Stop) -> Result<(String, bool)> {
    let mut finished = backend.receive_signal("Finished").await?;
    let mut owner = backend.receive_owner_changed().await?;
    let started: bool = backend.call("Start", &()).await?;
    if !started {
        return Err(
            "Window selection is unavailable. Close any active desktop overview and try again."
                .into(),
        );
    }
    diagnostic!("picalens backend=kwin mode=interactive");
    futures_util::select_biased! {
        reason = stop => {
            backend.call::<_, _, ()>("Stop", &()).await?;
            Ok((reason.unwrap_or("application shutdown").into(), true))
        }
        signal = finished.next().fuse() => {
            let signal = signal.ok_or("PicaLens lost the KWin backend")?;
            Ok(signal.body().deserialize()?)
        }
        _ = owner.next().fuse() => Err("KWin restarted during magnification".into()),
    }
}
