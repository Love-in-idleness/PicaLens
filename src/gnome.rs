use std::{cell::RefCell, collections::HashMap, rc::Rc};

use futures_channel::oneshot;
use futures_util::FutureExt;
use gtk::{gio, glib, prelude::*};

use crate::{
    locale::Locale,
    session::{Result, Stop, Target},
    settings::Layout,
};

const SHELL: &str = "org.gnome.Shell";
const PATH: &str = "/io/github/LoveInIdleness/PicaLens/Gnome";
const INTERFACE: &str = "io.github.LoveInIdleness.PicaLens.Gnome1";

async fn call(
    conn: &gio::DBusConnection,
    method: &str,
    arguments: Option<&glib::Variant>,
) -> std::result::Result<glib::Variant, glib::Error> {
    conn.call_future(
        Some(SHELL),
        PATH,
        INTERFACE,
        method,
        arguments,
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        2000,
    )
    .await
}

pub async fn available(conn: &gio::DBusConnection, layout: Layout) -> Result<bool> {
    match call(conn, "GetLayouts", None).await {
        Ok(layouts) => Ok(layouts
            .get::<(Vec<String>,)>()
            .is_some_and(|(layouts,)| layouts.iter().any(|item| item == layout.id()))),
        Err(error)
            if [
                gio::DBusError::ServiceUnknown,
                gio::DBusError::NameHasNoOwner,
                gio::DBusError::UnknownObject,
                gio::DBusError::UnknownInterface,
                gio::DBusError::UnknownMethod,
            ]
            .into_iter()
            .any(|kind| error.matches(kind)) =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn magnify(
    conn: &gio::DBusConnection,
    mut stop: Stop,
    layout: Layout,
    locale: Locale,
    target: Target,
) -> Result<String> {
    let (sender, finished) = oneshot::channel::<String>();
    let sender = Rc::new(RefCell::new(Some(sender)));
    let _finished = conn.subscribe_to_signal(
        Some(SHELL),
        Some(INTERFACE),
        Some("Finished"),
        Some(PATH),
        None,
        gio::DBusSignalFlags::NONE,
        {
            let sender = sender.clone();
            move |signal| {
                if let Some((reason,)) = signal.parameters.get::<(String,)>()
                    && let Some(sender) = sender.borrow_mut().take()
                {
                    let _ = sender.send(reason);
                }
            }
        },
    );
    let _owner = conn.subscribe_to_signal(
        Some("org.freedesktop.DBus"),
        Some("org.freedesktop.DBus"),
        Some("NameOwnerChanged"),
        Some("/org/freedesktop/DBus"),
        Some(SHELL),
        gio::DBusSignalFlags::NONE,
        move |_| {
            if let Some(sender) = sender.borrow_mut().take() {
                let _ = sender.send("gnome_unavailable".into());
            }
        },
    );
    let response = if target == Target::Active {
        match call(conn, "StartActive", Some(&(layout.id(),).to_variant())).await {
            Err(error) if error.matches(gio::DBusError::UnknownMethod) => {
                return Err("active_window_backend".into());
            }
            result => result?,
        }
    } else {
        let labels = [
            ("heading", "choose_window"),
            ("description", "gnome_choose_hint"),
            ("untitled", "gnome_untitled_window"),
            ("cancel", "cancel"),
        ]
        .into_iter()
        .map(|(key, id)| (key, locale.text(id)))
        .collect::<HashMap<_, _>>();
        call(conn, "Start", Some(&(layout.id(), labels).to_variant())).await?
    };
    let (error,) = response.get::<(String,)>().ok_or("gnome_unavailable")?;
    if !error.is_empty() {
        return Err(error.into());
    }
    diagnostic!("picalens backend=gnome mode=interactive");
    let reason = futures_util::select_biased! {
        reason = stop => {
            call(conn, "Stop", None).await?;
            reason.unwrap_or("application shutdown").to_owned()
        }
        reason = finished.fuse() => reason?,
    };
    if matches!(reason.as_str(), "gnome_source_lost" | "gnome_unavailable") {
        return Err(reason.into());
    }
    Ok(reason)
}
