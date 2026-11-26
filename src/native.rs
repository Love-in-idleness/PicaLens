use std::{os::unix::ffi::OsStrExt, path::Path, time::Duration};

use futures_util::FutureExt;
use gtk::{gio, glib, prelude::*};

use crate::{
    session::{Result, Stop, desktop_matches},
    settings::Filter,
};

pub const FINISHED: &str = "native presentation finished";

#[derive(Clone, Copy)]
pub enum Desktop {
    Niri,
    Hyprland,
}

impl Desktop {
    pub fn current() -> Option<Self> {
        if desktop_matches(&["niri"]) {
            Some(Self::Niri)
        } else if desktop_matches(&["Hyprland"]) {
            Some(Self::Hyprland)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Niri => "niri",
            Self::Hyprland => "hyprland",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Niri => "backend_niri",
            Self::Hyprland => "backend_hyprland",
        }
    }

    fn socket_address(self) -> Option<gio::UnixSocketAddress> {
        match self {
            Self::Niri => Some(gio::UnixSocketAddress::new(Path::new(&std::env::var_os(
                "NIRI_SOCKET",
            )?))),
            Self::Hyprland => {
                let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
                // Hyprland's runtime directory can exceed the pathname socket limit.
                let mut name =
                    format!("picalens-hypr-{}-", unsafe { libc::geteuid() }).into_bytes();
                name.extend_from_slice(instance.as_bytes());
                Some(gio::UnixSocketAddress::with_type(
                    UnixSocketAddressPath::Abstract(&name),
                ))
            }
        }
    }
}

pub struct Backend {
    desktop: Desktop,
    connection: gio::SocketConnection,
    replies: gio::DataInputStream,
}

impl Backend {
    async fn request(&self, line: &'static [u8]) -> Result<glib::collections::Slice<u8>> {
        let (_, _, error) = self
            .connection
            .output_stream()
            .write_all_future(line, glib::Priority::DEFAULT)
            .await
            .map_err(|(_, error)| error)?;
        if let Some(error) = error {
            return Err(error.into());
        }
        self.replies
            .read_line_future(glib::Priority::DEFAULT)
            .await?
            .ok_or_else(|| "native_backend_lost".into())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // The compositor ends the selection or presentation when this socket closes.
        let _ = self.connection.socket().close();
    }
}

pub async fn discover(desktop: Desktop, filter: Filter) -> Option<Backend> {
    let address = desktop.socket_address()?;
    let lookup = async {
        let connection = gio::SocketClient::new()
            .connect_future(&address)
            .await
            .ok()?;
        let uid = connection.socket().credentials().ok()?.unix_user().ok()?;
        if uid != unsafe { libc::geteuid() } {
            return None;
        }
        let replies = gio::DataInputStream::new(&connection.input_stream());
        let backend = Backend {
            desktop,
            connection,
            replies,
        };
        let reply = backend.request(b"\"PicaLensVersion\"\n").await.ok()?;
        let version = match &*reply {
            br#"{"Ok":{"PicaLensVersion":1}}"# => 1,
            br#"{"Ok":{"PicaLensVersion":2}}"# => 2,
            _ => return None,
        };
        (!filter.uses_gpu() || version >= 2).then_some(backend)
    };
    futures_util::select_biased! {
        backend = lookup.fuse() => backend,
        _ = glib::timeout_future(Duration::from_secs(2)).fuse() => None,
    }
}

pub async fn magnify(backend: Backend, mut stop: Stop, filter: Filter) -> Result<String> {
    let request: &'static [u8] = match filter {
        Filter::Fsr => b"{\"PicaLensStart\":{\"id\":null,\"filter\":\"fsr\"}}\n",
        Filter::Sgsr => b"{\"PicaLensStart\":{\"id\":null,\"filter\":\"sgsr\"}}\n",
        _ => b"{\"PicaLensStart\":{\"id\":null}}\n",
    };
    let start = backend.request(request);
    let reply = futures_util::select_biased! {
        reason = stop => return Ok(reason.unwrap_or("application shutdown").into()),
        reply = start.fuse() => reply?,
    };
    match &*reply {
        br#"{"Ok":"PicaLensCancelled"}"# => return Ok("selection cancelled".into()),
        br#"{"Err":"no_active_window"}"# => return Err("no_active_window".into()),
        br#"{"Err":"picalens_busy"}"# => return Err("native_backend_busy".into()),
        br#"{"Err":"interactive_filter_unavailable"}"# => {
            return Err("interactive_filter_unavailable".into());
        }
        br#"{"Ok":"Handled"}"# => {}
        _ => return Err("native_backend_lost".into()),
    }
    diagnostic!(
        "picalens backend={} mode=interactive filter={}",
        backend.desktop.name(),
        filter.id()
    );
    futures_util::select_biased! {
        reason = stop => Ok(reason.unwrap_or("application shutdown").into()),
        ended = backend.replies.read_line_future(glib::Priority::DEFAULT).fuse() => {
            match ended? {
                None => Ok(FINISHED.into()),
                Some(reply) if &*reply == br#"{"Err":"interactive_filter_unavailable"}"# => {
                    Err("interactive_filter_unavailable".into())
                }
                Some(_) => Err("native_backend_lost".into()),
            }
        },
    }
}
