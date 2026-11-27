use std::{future::Future, time::Duration};

use ashpd::{
    WindowIdentifier,
    desktop::{
        PersistMode, ResponseError, Session, SessionPortal,
        remote_desktop::{DeviceType, RemoteDesktop},
        screencast::{CursorMode, HasScreencastSession, Screencast, SourceType, Stream},
    },
    enumflags2::BitFlags,
    zbus,
};
use futures_channel::oneshot;
use futures_util::{FutureExt, StreamExt};
use gtk::prelude::IsA;

use crate::{capture::Capture, input::EisInput, preview::Preview};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub type Stop = oneshot::Receiver<&'static str>;
pub const PORTAL_CLOSED: &str = "portal session closed";
const WINDOW_PORTAL_DESKTOPS: &[&str] = &["niri", "Hyprland", "DDE", "Deepin", "Treeland"];
const UNSUPPORTED_INTERACTIVE_DESKTOPS: &[&str] = &["COSMIC", "Sway", "Denial"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Active,
    Choose,
}

pub fn portal_desktop() -> bool {
    desktop_matches(WINDOW_PORTAL_DESKTOPS)
}

pub fn interactive_desktop_unsupported() -> bool {
    desktop_matches(UNSUPPORTED_INTERACTIVE_DESKTOPS)
}

pub fn desktop_matches(names: &[&str]) -> bool {
    matches_desktop(
        &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        names,
    )
}

fn matches_desktop(desktop: &str, names: &[&str]) -> bool {
    desktop
        .split(':')
        .any(|desktop| names.iter().any(|name| desktop.eq_ignore_ascii_case(name)))
}

pub async fn diagnostics() -> Result<()> {
    for key in ["XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE", "WAYLAND_DISPLAY"] {
        println!("{key}={}", std::env::var(key).unwrap_or_default());
    }
    let bus = zbus::Connection::session().await?;
    for (portal, capability) in [
        ("ScreenCast", Some("AvailableSourceTypes")),
        ("RemoteDesktop", Some("AvailableDeviceTypes")),
        ("GlobalShortcuts", None),
    ] {
        let name = format!("org.freedesktop.portal.{portal}");
        let proxy = zbus::Proxy::new(
            &bus,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            name.as_str(),
        )
        .await?;
        match proxy.get_property::<u32>("version").await {
            Ok(version) => {
                println!("{portal} version={version}");
                if let Some(property) = capability {
                    match proxy.get_property::<u32>(property).await {
                        Ok(bits) => println!("{portal} {property}={bits:#x}"),
                        Err(error) => println!("{portal} {property}: {error}"),
                    }
                }
            }
            Err(error) => println!("{portal} unavailable: {error}"),
        }
    }
    Ok(())
}

pub async fn run(view: &Preview, experiment: bool, stop: Stop) -> Result<&'static str> {
    if !experiment {
        return capture_window(Some(view.window.as_ref()), stop, |capture, size| {
            view.show(capture, None, size)
        })
        .await;
    }
    let cast = window_portal().await?;
    let remote = RemoteDesktop::new().await?;
    let available = remote.available_device_types().await?;
    if !available.contains(DeviceType::Pointer) {
        return Err("RemoteDesktop does not advertise pointer access".into());
    }
    let devices = available & (DeviceType::Pointer | DeviceType::Keyboard);
    let session = remote.create_session().await?;
    with_session(&session, stop, async {
        diagnostic!("portal RemoteDesktop.SelectDevices requested_devices={devices:?}");
        remote
            .select_devices(&session, devices, None, PersistMode::DoNot)
            .await?
            .response()?;
        select_window(&cast, &session).await?;
        let parent = parent(&view.window).await;
        diagnostic!("portal RemoteDesktop.Start");
        let selected = remote.start(&session, parent.as_ref()).await?.response()?;
        let granted = selected.devices();
        diagnostic!("portal granted_devices={granted:?}");
        let stream = window_stream(selected.streams().unwrap_or_default(), false)?;
        let capture = open_capture(&cast, &session, stream).await?;
        let input = match connect_input(&remote, &session, stream, granted).await {
            Ok(input) => Some(input),
            Err(error) => {
                diagnostic!("input unavailable: {error}");
                None
            }
        };
        view.show(capture, input, stream.size()).await
    })
    .await
}

async fn window_portal() -> Result<Screencast<'static>> {
    let cast = Screencast::new().await.map_err(|error| {
        diagnostic!("ScreenCast portal unavailable: {error}");
        "window_portal_unavailable"
    })?;
    match cast.available_source_types().await {
        Ok(types) if types.contains(SourceType::Window) => Ok(cast),
        Ok(types) => {
            diagnostic!("ScreenCast does not advertise window sources: {types:?}");
            Err("window_portal_unavailable".into())
        }
        Err(error) => {
            diagnostic!("ScreenCast.AvailableSourceTypes: {error}");
            Err("window_portal_unavailable".into())
        }
    }
}

pub async fn capture_window(
    window: Option<&gtk::Native>,
    stop: Stop,
    show: impl AsyncFnOnce(Capture, Option<(i32, i32)>) -> Result<&'static str>,
) -> Result<&'static str> {
    let cast = window_portal().await?;
    let session = cast.create_session().await?;
    with_session(&session, stop, async {
        select_window(&cast, &session).await?;
        let parent = match window {
            Some(window) => parent(window).await,
            None => None,
        };
        diagnostic!("portal ScreenCast.Start");
        let selected = cast.start(&session, parent.as_ref()).await?.response()?;
        let legacy_types =
            desktop_matches(&["Hyprland"]) && hyprland_legacy_types(&session).await?;
        let stream = window_stream(selected.streams(), legacy_types)?;
        let capture = open_capture(&cast, &session, stream).await?;
        show(capture, stream.size()).await
    })
    .await
}

async fn parent(window: &impl IsA<gtk::Native>) -> Option<WindowIdentifier> {
    let parent = futures_util::future::select(
        Box::pin(WindowIdentifier::from_native(window)),
        glib_timeout(Duration::from_secs(2)),
    )
    .await;
    let id = match parent {
        futures_util::future::Either::Left((id, _)) => id,
        futures_util::future::Either::Right(_) => None,
    };
    diagnostic!("portal wayland_parent_exported={}", id.is_some());
    id
}

fn glib_timeout(duration: Duration) -> std::pin::Pin<Box<impl Future<Output = ()>>> {
    Box::pin(gtk::glib::timeout_future(duration))
}

async fn with_session<T: SessionPortal>(
    session: &Session<'_, T>,
    mut stop: Stop,
    operation: impl Future<Output = Result<&'static str>>,
) -> Result<&'static str> {
    let result: Result<_> = async {
        let closed = session.receive_closed().await?;
        futures_util::pin_mut!(closed);
        futures_util::select_biased! {
            reason = stop => Ok((reason.unwrap_or("application shutdown"), false)),
            _ = closed.next().fuse() => Ok((PORTAL_CLOSED, true)),
            outcome = operation.fuse() => outcome.map(|reason| (reason, false)),
        }
    }
    .await;

    if !matches!(result, Ok((_, true))) {
        let close = futures_util::future::select(
            Box::pin(session.close()),
            glib_timeout(Duration::from_secs(2)),
        )
        .await;
        use futures_util::future::Either;
        match close {
            Either::Left((Ok(()), _)) => {}
            Either::Left((Err(error), _)) => diagnostic!("portal close: {error}"),
            Either::Right(_) => diagnostic!("portal close timed out"),
        }
    }
    match result {
        Err(error)
            if matches!(
                error.downcast_ref::<ashpd::Error>(),
                Some(ashpd::Error::Response(ResponseError::Cancelled))
            ) =>
        {
            Ok("portal selection cancelled")
        }
        other => other.map(|(reason, _)| reason),
    }
}

async fn select_window(
    cast: &Screencast<'_>,
    session: &Session<'_, impl HasScreencastSession>,
) -> Result<()> {
    diagnostic!("portal ScreenCast.SelectSources requested_source=Window");
    cast.select_sources(
        session,
        CursorMode::Hidden,
        SourceType::Window.into(),
        false,
        None,
        PersistMode::DoNot,
    )
    .await?
    .response()?;
    Ok(())
}

async fn hyprland_legacy_types(session: &Session<'_, Screencast<'_>>) -> Result<bool> {
    use zbus::{fdo, zvariant};

    const HYPRLAND: &str = "org.freedesktop.impl.portal.desktop.hyprland";
    let bus = zbus::connection::Builder::session()?
        .method_timeout(Duration::from_secs(2))
        .build()
        .await?;
    let dbus = fdo::DBusProxy::new(&bus).await?;
    let owner = match dbus.get_name_owner(HYPRLAND.try_into()?).await {
        Ok(owner) => owner,
        Err(fdo::Error::NameHasNoOwner(_)) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let backend = zbus::Proxy::new(
        &bus,
        owner.clone(),
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.impl.portal.ScreenCast",
    )
    .await?;
    if backend.get_property::<u32>("version").await? != 3 {
        return Ok(false);
    }

    // Another backend may handle ScreenCast while Hyprland serves different portals.
    // Check that this particular session belongs to the legacy backend.
    let context = zvariant::serialized::Context::new_dbus(zvariant::LE, 0);
    let (path, _) =
        zvariant::to_bytes(context, session)?.deserialize::<zvariant::OwnedObjectPath>()?;
    let object = fdo::IntrospectableProxy::builder(&bus)
        .destination(owner)?
        .path(path)?
        .build()
        .await?;
    match object.introspect().await {
        Ok(xml) => Ok(xml.contains("name=\"org.freedesktop.impl.portal.Session\"")),
        Err(fdo::Error::UnknownObject(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn is_window_source(source: Option<SourceType>, hyprland_legacy: bool) -> bool {
    if hyprland_legacy {
        // Hyprland's ScreenCast v3 shifted the flags again in its stream response.
        source == Some(SourceType::Virtual)
    } else {
        source.is_none_or(|kind| kind == SourceType::Window)
    }
}

fn window_stream(streams: &[Stream], hyprland_legacy: bool) -> Result<&Stream> {
    let [stream] = streams else {
        return Err(format!("expected one window stream, received {}", streams.len()).into());
    };
    diagnostic!(
        "portal requested_source=Window reported_source={:?} hyprland_legacy_types={hyprland_legacy} node={} logical_size={:?} mapping_id={:?}",
        stream.source_type(),
        stream.pipe_wire_node_id(),
        stream.size(),
        stream.mapping_id(),
    );
    if !is_window_source(stream.source_type(), hyprland_legacy) {
        return Err("portal_non_window".into());
    }
    Ok(stream)
}

async fn open_capture(
    cast: &Screencast<'_>,
    session: &Session<'_, impl HasScreencastSession>,
    stream: &Stream,
) -> Result<Capture> {
    let fd = cast.open_pipe_wire_remote(session).await?;
    Ok(crate::capture::start(fd, stream.pipe_wire_node_id())?)
}

async fn connect_input(
    remote: &RemoteDesktop<'_>,
    session: &Session<'_, RemoteDesktop<'_>>,
    stream: &Stream,
    granted: BitFlags<DeviceType>,
) -> Result<EisInput> {
    if stream.source_type() != Some(SourceType::Window) || !granted.contains(DeviceType::Pointer) {
        return Err("portal did not confirm both Window and Pointer".into());
    }
    let mapping = stream
        .mapping_id()
        .filter(|id| !id.is_empty())
        .ok_or("window stream has no mapping_id")?;
    let fd = remote.connect_to_eis(session).await?;
    Ok(EisInput::start(
        fd,
        mapping.into(),
        granted.contains(DeviceType::Keyboard),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_complete_desktop_names() {
        for desktop in ["niri", "Hyprland", "DDE", "deepin:DDE", "treeland"] {
            assert!(
                matches_desktop(desktop, WINDOW_PORTAL_DESKTOPS),
                "{desktop}"
            );
        }
        for desktop in ["", "GNOME", "KDE", "NotDeepin"] {
            assert!(
                !matches_desktop(desktop, WINDOW_PORTAL_DESKTOPS),
                "{desktop}"
            );
        }
    }

    #[test]
    fn matches_unsupported_interactive_desktops_by_complete_name() {
        for desktop in ["COSMIC", "sway", "Denial", "distribution:Cosmic"] {
            assert!(matches_desktop(desktop, UNSUPPORTED_INTERACTIVE_DESKTOPS));
        }
        for desktop in ["", "pop:GNOME", "KDE", "niri", "Hyprland", "DDE", "NotSway"] {
            assert!(!matches_desktop(desktop, UNSUPPORTED_INTERACTIVE_DESKTOPS));
        }
    }

    #[test]
    fn hyprland_v3_decodes_windows_and_rejects_shifted_monitor_flags() {
        assert!(is_window_source(Some(SourceType::Virtual), true));
        assert!(!is_window_source(Some(SourceType::Window), true));
        assert!(!is_window_source(None, true));
        assert!(is_window_source(Some(SourceType::Window), false));
        assert!(!is_window_source(Some(SourceType::Virtual), false));
        assert!(!is_window_source(Some(SourceType::Monitor), false));
    }
}
