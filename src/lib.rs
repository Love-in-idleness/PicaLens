// A desktop launcher may close its log pipe while the application is running.
macro_rules! diagnostic {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), $($arg)*);
    }};
}

mod capture;
mod input;
mod kwin;
mod preview;
mod probe;
mod session;
mod standalone;

pub use probe::run as run_probe;
pub use standalone::run;
