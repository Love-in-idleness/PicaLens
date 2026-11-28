use std::{collections::HashMap, path::Path, time::Duration};

use ashpd::zbus::{self, Proxy, fdo::DBusProxy, zvariant::Value};
use futures_util::{FutureExt, StreamExt};
use gtk::{gio, glib, prelude::*};

use crate::{
    session::{Result, Stop},
    settings::{Filter, Layout, Preset},
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

pub async fn supports_filter(backend: &Proxy<'_>, filter: Filter) -> Result<bool> {
    if filter == Filter::Linear {
        return Ok(true);
    }
    match backend
        .call::<_, _, Vec<String>>("SupportedFilters", &())
        .await
    {
        Ok(filters) => Ok(filters.iter().any(|id| id == filter.id())),
        Err(zbus::Error::MethodError(name, ..))
            if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn supports_render_worker(backend: &Proxy<'_>) -> Result<bool> {
    match backend.call::<_, _, i32>("RenderWorkerVersion", &()).await {
        Ok(1) => Ok(true),
        Ok(_) => Ok(false),
        Err(zbus::Error::MethodError(name, ..))
            if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" =>
        {
            Ok(false)
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

pub async fn active_app_id(conn: &gio::DBusConnection) -> Option<String> {
    let reply = active_call(conn, "ActiveWindowAppId", None).await.ok()?;
    let (app,) = reply.get::<(String,)>()?;
    (!app.is_empty()).then_some(app)
}

pub async fn capture_screenshot(conn: &gio::DBusConnection, path: &Path) -> Result<bool> {
    let path = path
        .to_str()
        .ok_or("screenshot path is not valid UTF-8")?;
    let reply = active_call(conn, "CaptureScreenshot", Some(&(path,).to_variant())).await?;
    Ok(reply.get::<(bool,)>().is_some_and(|(ok,)| ok))
}

fn gio_options(preset: &Preset, hide_cursor_seconds: u32) -> glib::Variant {
    let dict = glib::VariantDict::new(None);
    dict.insert("layout", preset.layout.id());
    dict.insert("filter", preset.filter.id());
    dict.insert("renderDevice", preset.render_device.as_str());
    dict.insert("cropLeft", preset.crop.left as i32);
    dict.insert("cropTop", preset.crop.top as i32);
    dict.insert("cropRight", preset.crop.right as i32);
    dict.insert("cropBottom", preset.crop.bottom as i32);
    dict.insert("hideCursorMs", hide_cursor_seconds.saturating_mul(1000) as i32);
    (dict.end(),).to_variant()
}

fn zbus_options<'a>(preset: &'a Preset, hide_cursor_seconds: u32) -> HashMap<&'static str, Value<'a>> {
    HashMap::from([
        ("layout", Value::from(preset.layout.id())),
        ("filter", Value::from(preset.filter.id())),
        ("renderDevice", Value::from(preset.render_device.as_str())),
        ("cropLeft", Value::from(preset.crop.left as i32)),
        ("cropTop", Value::from(preset.crop.top as i32)),
        ("cropRight", Value::from(preset.crop.right as i32)),
        ("cropBottom", Value::from(preset.crop.bottom as i32)),
        (
            "hideCursorMs",
            Value::from(hide_cursor_seconds.saturating_mul(1000) as i32),
        ),
    ])
}

pub async fn magnify(
    backend: Proxy<'_>,
    mut stop: Stop,
    preset: &Preset,
    hide_cursor_seconds: u32,
    active: Option<&gio::DBusConnection>,
) -> Result<String> {
    let Preset {
        layout,
        filter,
        render_device,
        crop,
        ..
    } = preset;
    let mut finished = backend.receive_signal("Finished").await?;
    let mut owner = backend.receive_owner_changed().await?;
    let started: bool = if let Some(conn) = active {
        let options = gio_options(preset, hide_cursor_seconds);
        match active_call(conn, "StartActiveWithOptions", Some(&options)).await {
            Ok(reply) => reply.get::<(bool,)>().ok_or("active_window_backend")?.0,
            Err(error) if error.matches(gio::DBusError::UnknownMethod) => {
                if !crop.is_empty() {
                    return Err("kwin_upgrade".into());
                }
                let (method, args) = if preset.uses_render_worker() {
                    (
                        "StartActiveWithRenderDevice",
                        (layout.id(), filter.id(), render_device.as_str()).to_variant(),
                    )
                } else if filter.uses_gpu() {
                    (
                        "StartActiveWithFilter",
                        (layout.id(), filter.id()).to_variant(),
                    )
                } else {
                    ("StartActive", (layout.id(),).to_variant())
                };
                match active_call(conn, method, Some(&args)).await {
                    Ok(reply) => reply.get::<(bool,)>().ok_or("active_window_backend")?.0,
                    Err(error) if error.matches(gio::DBusError::UnknownMethod) => {
                        return Err("active_window_backend".into());
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
    } else {
        let options = zbus_options(preset, hide_cursor_seconds);
        match backend
            .call::<_, _, bool>("StartWithOptions", &(options,))
            .await
        {
            Ok(started) => started,
            Err(zbus::Error::MethodError(name, ..))
                if name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" =>
            {
                if !crop.is_empty() {
                    return Err("kwin_upgrade".into());
                }
                if preset.uses_render_worker() {
                    backend
                        .call(
                            "StartWithRenderDevice",
                            &(layout.id(), filter.id(), render_device.as_str()),
                        )
                        .await?
                } else if filter.uses_gpu() {
                    backend
                        .call("StartWithFilter", &(layout.id(), filter.id()))
                        .await?
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
                }
            }
            Err(error) => return Err(error.into()),
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
    diagnostic!(
        "picalens backend=kwin mode=interactive filter={}",
        filter.id()
    );
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
            if matches!(reason.as_str(), "interactive_filter_unavailable" | "render_gpu_missing"
                | "render_gpu_driver" | "render_size" | "render_worker_failed" | "invalid_crop") {
                return Err(reason.into());
            }
            Ok(reason)
        }
        _ = owner.next().fuse() => Err("KWin restarted during magnification".into()),
    }
}
