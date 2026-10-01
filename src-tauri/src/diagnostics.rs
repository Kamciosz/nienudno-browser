//! Test-build diagnostics: app.log, panic backtraces. No-ops in the private build.

#[cfg(not(feature = "private"))]
use std::fs::{self, File, OpenOptions};
#[cfg(not(feature = "private"))]
use std::io::Write;
use std::path::Path;
#[cfg(not(feature = "private"))]
use std::path::PathBuf;
#[cfg(not(feature = "private"))]
use std::sync::Mutex;
#[cfg(not(feature = "private"))]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(not(feature = "private"))]
static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);

#[cfg(not(feature = "private"))]
pub fn logs_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

pub fn init(data_dir: &Path) {
    #[cfg(not(feature = "private"))]
    {
        let dir = logs_dir(data_dir);
        if fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = dir.join("app.log");
        if fs::metadata(&path)
            .map(|meta| meta.len() > 1_000_000)
            .unwrap_or(false)
        {
            let _ = fs::remove_file(&path);
        }
        if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
            *LOG_FILE.lock().unwrap() = Some(file);
        }
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log(&format!("PANIC: {info}"));
            log(&format!(
                "backtrace:\n{}",
                std::backtrace::Backtrace::force_capture()
            ));
            previous(info);
        }));
        log("=== app start ===");
    }
    #[cfg(feature = "private")]
    let _ = data_dir;
}

pub fn log(message: &str) {
    #[cfg(not(feature = "private"))]
    if let Ok(mut guard) = LOG_FILE.lock() {
        if let Some(file) = guard.as_mut() {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or(0);
            let _ = writeln!(file, "[{stamp}] {message}");
            let _ = file.flush();
        }
    }
    #[cfg(feature = "private")]
    let _ = message;
}
