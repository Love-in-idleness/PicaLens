use std::time::Duration;

use ashpd::zbus::{self, Proxy, fdo::DBusProxy};
use futures_util::{FutureExt, StreamExt};
use gtk::{gio, glib, prelude::*};

use crate::{
    session::{Result, Stop},
    settings::Layout,
};

const SERVICE: &str = "org.kde.KWin";
const EFFECT: &str = "picalens";
const PATH: &str = "/io/github/LoveInIdleness/PicaLens/Backend";
const INTERFACE: &str = "io.github.LoveInIdleness.PicaLens.Backend1";

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
    Ok(Some(Proxy::new(&bus, SERVICE, PATH, INTERFACE).await?))
}

pub async fn supports_layout(backend: &Proxy<'_>, layout: Layout) -> Result<bool> {
    if layout == Layout::Fit {
        return Ok(true);
    }
    match backend
        .call::<_, _, Vec<String>>("SupportedLayouts", &())
        .await
    {
        Ok(layouts) => Ok(layouts.iter().any(|id| id == layout.id())),
        Err(zbus::Error::MethodError(name, ..))
            if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" =>
        {
            Ok(layout == Layout::Stretch)
        }
        Err(error) => Err(error.into()),
    }
}

async fn active_call(
    conn: &gio::DBusConnection,
    method: &str,
    arguments: Option<&glib::Variant>,
) -> std::result::Result<glib::Variant, glib::Error> {
    // KWin authenticates the shortcut request against the GApplication owner.
    conn.call_future(
        Some(SERVICE),
        PATH,
        INTERFACE,
        method,
        arguments,
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        3000,
    )
    .await
}

pub async fn magnify(
    backend: Proxy<'_>,
    mut stop: Stop,
    layout: Layout,
    active: Option<&gio::DBusConnection>,
) -> Result<String> {
    let mut finished = backend.receive_signal("Finished").await?;
    let mut owner = backend.receive_owner_changed().await?;
    let started: bool = if let Some(conn) = active {
        match active_call(conn, "StartActive", Some(&(layout.id(),).to_variant())).await {
            Ok(reply) => reply.get::<(bool,)>().ok_or("active_window_backend")?.0,
            Err(error) if error.matches(gio::DBusError::UnknownMethod) => {
                return Err("active_window_backend".into());
            }
            Err(error) => return Err(error.into()),
        }
    } else {
        match layout {
            Layout::Fit => backend.call("Start", &()).await?,
            _ => match backend.call("StartWithLayout", &(layout.id(),)).await {
                Err(zbus::Error::MethodError(name, ..))
                    if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" =>
                {
                    return Err("kwin_upgrade".into());
                }
                result => result?,
            },
        }
    };
    if !started {
        if active.is_some() {
            return Err("no_active_window".into());
        }
        return Err(
            "Window selection is unavailable. Close any active desktop overview and try again."
                .into(),
        );
    }
    diagnostic!("picalens backend=kwin mode=interactive");
    futures_util::select_biased! {
        reason = stop => {
            if let Some(conn) = active {
                active_call(conn, "Stop", None).await?;
            } else {
                backend.call::<_, _, ()>("Stop", &()).await?;
            }
            Ok(reason.unwrap_or("application shutdown").into())
        }
        signal = finished.next().fuse() => {
            let signal = signal.ok_or("PicaLens lost the KWin backend")?;
            let (reason, _): (String, bool) = signal.body().deserialize()?;
            Ok(reason)
        }
        _ = owner.next().fuse() => Err("KWin restarted during magnification".into()),
    }
}
