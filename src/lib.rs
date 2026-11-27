// A desktop launcher may close its log pipe while the application is running.
macro_rules! diagnostic {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), $($arg)*);
    }};
}

mod capture;
mod game_page;
mod gamescope;
mod gnome;
mod gpu;
mod input;
mod kwin;
mod locale;
mod native;
mod preview;
mod probe;
mod render_client;
mod render_device;
mod render_worker;
mod session;
mod settings;
mod shortcut;
mod standalone;
mod upscale;
mod viewport;

pub use probe::run as run_probe;
pub use render_worker::run as run_render_worker;
pub use standalone::run;
