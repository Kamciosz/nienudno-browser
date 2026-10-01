use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::Duration,
};

use cef::*;
use percent_encoding::{percent_encode, NON_ALPHANUMERIC};
#[cfg(target_os = "linux")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tauri::{AppHandle, Emitter, Manager, Window};

use crate::{BrowserState, HOME_PAGE};

// The new-tab page inlined into a data: URL (no scheme handler in CEF, and
// update_url keeps data:text/html out of tab history so the bar shows
// nienudno://start).
fn start_page() -> String {
    let svg = include_str!("../../ui/mark.svg").replacen(
        "<svg ",
        r#"<svg class="mark" width="80" height="80" "#,
        1,
    );
    let html = include_str!("../../ui/start.html")
        .replace(
            r#"<link rel="stylesheet" href="start.css">"#,
            &format!("<style>{}</style>", include_str!("../../ui/start.css")),
        )
        .replace(
            r#"<script src="start.js" defer></script>"#,
            &format!("<script>{}</script>", include_str!("../../ui/start.js")),
        )
        .replace(
            r#"<img class="mark" src="mark.svg" alt="" width="80" height="80">"#,
            &svg,
        );
    format!(
        "data:text/html,{}",
        percent_encode(html.as_bytes(), NON_ALPHANUMERIC)
    )
}

static CEF_SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

pub(crate) fn prepare_shutdown() {
    CEF_SHUTTING_DOWN.store(true, Ordering::Release);
}

#[derive(Clone, Copy)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy)]
pub struct ParentHandle(pub cef::sys::cef_window_handle_t);

struct CefHandler {
    app: AppHandle,
    state: BrowserState,
    browsers: BTreeMap<u64, Browser>,
    browser_tabs: BTreeMap<i32, u64>,
    #[cfg(target_os = "macos")]
    message_pump_scheduled: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct CefHost {
    inner: Arc<Mutex<CefHandler>>,
}

impl CefHost {
    pub fn initialize(
        app: AppHandle,
        state: BrowserState,
        data_dir: &Path,
        resource_dir: &Path,
    ) -> Result<Self, String> {
        CEF_SHUTTING_DOWN.store(false, Ordering::Release);
        load_cef_library(false)?;
        let inner = Arc::new(Mutex::new(CefHandler {
            app,
            state,
            browsers: BTreeMap::new(),
            browser_tabs: BTreeMap::new(),
            #[cfg(target_os = "macos")]
            message_pump_scheduled: Arc::new(AtomicBool::new(false)),
        }));
        let mut cef_app = BrowserApp::new(inner.clone());
        let cache_path = CefString::from(
            data_dir
                .join("cef-profile")
                .to_str()
                .ok_or_else(|| "Nieprawidłowa ścieżka profilu CEF.".to_string())?,
        );
        let (resources_dir_path, locales_dir_path, framework_dir_path) =
            resource_paths(resource_dir)?;
        let browser_subprocess_path = browser_subprocess_path()?;
        let args = cef::args::Args::new();
        #[cfg(not(feature = "private"))]
        let log_file = {
            let log_dir = data_dir.join("logs");
            let _ = std::fs::create_dir_all(&log_dir);
            let log_path = log_dir.join("cef.log").to_string_lossy().into_owned();
            CefString::from(log_path.as_str())
        };
        let settings = Settings {
            no_sandbox: 1,
            browser_subprocess_path,
            #[cfg(not(feature = "private"))]
            log_severity: LogSeverity::INFO,
            #[cfg(feature = "private")]
            log_severity: LogSeverity::DISABLE,
            #[cfg(not(feature = "private"))]
            log_file,
            cache_path,
            resources_dir_path,
            locales_dir_path,
            framework_dir_path,
            persist_session_cookies: 1,
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            multi_threaded_message_loop: 1,
            #[cfg(target_os = "macos")]
            external_message_pump: 1,
            ..Settings::default()
        };
        if initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut cef_app),
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(format!(
                "Nie udało się uruchomić silnika CEF (kod {}).",
                cef::get_exit_code()
            ));
        }
        Ok(Self { inner })
    }

    pub fn create_browser(
        &self,
        window: &Window,
        tab_id: u64,
        url: &str,
        private_mode: bool,
        tor_mode: bool,
    ) -> Result<(), String> {
        let parent = parent_handle(window)?;
        let bounds = browser_bounds(window)?;
        let mut client = BrowserClient::new(self.inner.clone(), Some(tab_id));
        let mut window_info = WindowInfo::default();
        // The window content view is not flipped, so the initial child rect uses
        // a bottom-left origin; layout() re-asserts this once the view exists.
        #[cfg(target_os = "macos")]
        let bounds_y =
            (content_view_size(window)?.1 - bounds.y as f64 - bounds.height as f64).max(0.0) as i32;
        #[cfg(not(target_os = "macos"))]
        let bounds_y = bounds.y;
        let cef_bounds = Rect {
            x: bounds.x,
            y: bounds_y,
            width: bounds.width,
            height: bounds.height,
        };
        window_info = window_info.set_as_child(parent.0, &cef_bounds);
        let browser_settings = BrowserSettings::default();
        let mut context = if private_mode || tor_mode {
            let context_settings = RequestContextSettings {
                persist_session_cookies: 0,
                ..RequestContextSettings::default()
            };
            let context = request_context_create_context(Some(&context_settings), None)
                .ok_or_else(|| "CEF nie utworzył prywatnego profilu.".to_string())?;
            if tor_mode {
                configure_tor_proxy(&context)?;
            }
            Some(context)
        } else {
            None
        };
        let start = start_page();
        let target = CefString::from(if url == HOME_PAGE {
            start.as_str()
        } else {
            url
        });
        let result = browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            Some(&target),
            Some(&browser_settings),
            None,
            context.as_mut(),
        );
        if result == 0 {
            return Err("CEF nie utworzył karty.".to_string());
        }
        Ok(())
    }

    pub fn navigate(&self, tab_id: u64, url: &str) -> Result<(), String> {
        let browser = self.browser(tab_id)?;
        let start = start_page();
        let target = CefString::from(if url == HOME_PAGE {
            start.as_str()
        } else {
            url
        });
        browser
            .main_frame()
            .ok_or_else(|| "Karta nie ma głównej ramki.".to_string())?
            .load_url(Some(&target));
        Ok(())
    }

    pub fn back(&self, tab_id: u64) -> Result<(), String> {
        self.browser(tab_id)?.go_back();
        Ok(())
    }

    pub fn forward(&self, tab_id: u64) -> Result<(), String> {
        self.browser(tab_id)?.go_forward();
        Ok(())
    }

    pub fn reload(&self, tab_id: u64) -> Result<(), String> {
        self.browser(tab_id)?.reload();
        Ok(())
    }

    pub fn close(&self, tab_id: u64) -> Result<(), String> {
        let browser = self.browser(tab_id)?;
        browser
            .host()
            .ok_or_else(|| "Karta nie ma hosta CEF.".to_string())?
            .close_browser(1);
        Ok(())
    }

    pub fn open_devtools(&self, tab_id: u64, _window: &Window) -> Result<(), String> {
        let browser = self.browser(tab_id)?;
        let host = browser
            .host()
            .ok_or_else(|| "Karta nie ma hosta CEF.".to_string())?;
        let mut client = BrowserClient::new(self.inner.clone(), None);
        host.show_dev_tools(
            None,
            Some(&mut client),
            Some(&BrowserSettings::default()),
            None,
        );
        Ok(())
    }

    /// Close every browser (and its DevTools) and pump CEF until they are
    /// gone, so `cef::shutdown()` never runs with live browsers. Must be
    /// called while the main window — and with it the CEF child views —
    /// still exist (window CloseRequested), otherwise `do_close` would
    /// detach already-freed views.
    pub fn close_all_sync(&self) {
        let hosts: Vec<_> = self
            .inner
            .lock()
            .expect("CEF lock poisoned")
            .browsers
            .values()
            .filter_map(|browser| browser.host())
            .collect();
        for host in hosts {
            host.close_dev_tools();
            host.close_browser(1);
        }
        for _ in 0..180 {
            let empty = self
                .inner
                .lock()
                .expect("CEF lock poisoned")
                .browsers
                .is_empty();
            if empty {
                break;
            }
            thread::sleep(Duration::from_millis(16));
            cef::do_message_loop_work();
        }
        // Grace pumps so DevTools teardown (not tracked in the map) finishes too.
        for _ in 0..15 {
            thread::sleep(Duration::from_millis(16));
            cef::do_message_loop_work();
        }
    }

    pub fn layout(
        &self,
        window: &Window,
        active_tab: Option<u64>,
        panel_open: bool,
    ) -> Result<(), String> {
        let bounds = browser_bounds(window)?;
        let browsers = self
            .inner
            .lock()
            .expect("CEF lock poisoned")
            .browsers
            .iter()
            .map(|(tab_id, browser)| (*tab_id, browser.clone()))
            .collect::<Vec<_>>();
        for (tab_id, browser) in browsers {
            let host = browser
                .host()
                .ok_or_else(|| "Karta nie ma hosta CEF.".to_string())?;
            let handle = host.window_handle();
            set_native_bounds(handle, &bounds)?;
            set_native_visible(handle, Some(tab_id) == active_tab && !panel_open)?;
        }
        Ok(())
    }

    fn browser(&self, tab_id: u64) -> Result<Browser, String> {
        self.inner
            .lock()
            .expect("CEF lock poisoned")
            .browsers
            .get(&tab_id)
            .cloned()
            .ok_or_else(|| "Nie znaleziono karty CEF.".to_string())
    }
}

fn resource_paths(resource_dir: &Path) -> Result<(CefString, CefString, CefString), String> {
    #[cfg(target_os = "macos")]
    {
        let _ = resource_dir;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let bundled_framework = executable
            .parent()
            .ok_or_else(|| "Nieprawidłowa ścieżka programu.".to_string())?
            .join("../Frameworks")
            .join(cef::sys::FRAMEWORK_PATH);
        let framework_binary = if bundled_framework.exists() {
            bundled_framework
        } else {
            // Dev build: framework lives in the CEF download dir, not an app bundle.
            // framework_dir_path makes CEF resolve icudtl.dat/paks via the framework
            // bundle instead of NSBundle.mainBundle (which has no Resources here).
            cef::sys::get_cef_dir()
                .ok_or_else(|| "Nie znaleziono zasobów CEF dla tej architektury.".to_string())?
                .join(cef::sys::FRAMEWORK_PATH)
        };
        let framework = framework_binary
            .parent()
            .ok_or_else(|| "Nieprawidłowa ścieżka frameworku CEF.".to_string())?
            .to_path_buf();
        let resources = framework.join("Resources");
        let locales_path = resources.join("locales");
        let framework = CefString::from(
            framework
                .to_str()
                .ok_or_else(|| "Nieprawidłowa ścieżka frameworku CEF.".to_string())?,
        );
        let resources = CefString::from(
            resources
                .to_str()
                .ok_or_else(|| "Nieprawidłowa ścieżka zasobów CEF.".to_string())?,
        );
        let locales = CefString::from(
            locales_path
                .to_str()
                .ok_or_else(|| "Nieprawidłowa ścieżka języków CEF.".to_string())?,
        );
        Ok((resources, locales, framework))
    }
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        let runtime_dir = cef_runtime_dir(resource_dir)?;
        let locales_dir = runtime_dir.join("locales");
        let resources = CefString::from(
            runtime_dir
                .to_str()
                .ok_or_else(|| "Nieprawidłowa ścieżka zasobów CEF.".to_string())?,
        );
        let locales = CefString::from(
            locales_dir
                .to_str()
                .ok_or_else(|| "Nieprawidłowa ścieżka języków CEF.".to_string())?,
        );
        Ok((resources, locales, CefString::from("")))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = resource_dir;
        Ok((
            CefString::from(""),
            CefString::from(""),
            CefString::from(""),
        ))
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
fn cef_runtime_dir(resource_dir: &Path) -> Result<std::path::PathBuf, String> {
    let executable_dir = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .parent()
        .map(Path::to_path_buf);
    let candidates = [
        Some(resource_dir.to_path_buf()),
        Some(resource_dir.join("cef-bundle")),
        executable_dir,
        cef::sys::get_cef_dir(),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|path| path.join("icudtl.dat").is_file() && path.join("locales").is_dir())
        .ok_or_else(|| "Nie znaleziono zasobów CEF w paczce aplikacji.".to_string())
}

fn browser_subprocess_path() -> Result<CefString, String> {
    #[cfg(target_os = "macos")]
    {
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let macos_dir = executable
            .parent()
            .ok_or_else(|| "Nieprawidłowa ścieżka programu.".to_string())?;
        let contents_dir = macos_dir
            .parent()
            .ok_or_else(|| "Nieprawidłowa ścieżka pakietu macOS.".to_string())?;
        let app_dir = contents_dir
            .parent()
            .ok_or_else(|| "Nieprawidłowa ścieżka aplikacji macOS.".to_string())?;
        let helper = app_dir
            .join("Contents/Frameworks/NieNudno Browser Helper.app/Contents/MacOS")
            .join("NieNudno Browser Helper");
        let is_app_bundle = contents_dir
            .file_name()
            .is_some_and(|name| name == "Contents");
        if !is_app_bundle {
            // Dev build: CEF CHECKs a non-empty subprocess path on macOS, so point
            // it at the sibling helper binary instead of an app-bundle path.
            let dev_helper = macos_dir.join("nienudno-browser-helper");
            return Ok(match dev_helper.is_file() {
                true => CefString::from(dev_helper.to_string_lossy().as_ref()),
                false => CefString::from(""),
            });
        }
        if !helper.is_file() {
            return Err(format!(
                "Nie znaleziono procesu pomocniczego CEF: {}",
                helper.display()
            ));
        }
        Ok(CefString::from(helper.to_string_lossy().as_ref()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(CefString::from(""))
    }
}

static CEF_LIBRARY_LOADED: OnceLock<Result<(), String>> = OnceLock::new();

pub fn load_cef_library(helper: bool) -> Result<(), String> {
    CEF_LIBRARY_LOADED
        .get_or_init(|| load_cef_library_once(helper))
        .clone()
}

fn load_cef_library_once(helper: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};

        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let bundled_framework = executable
            .parent()
            .ok_or_else(|| "Nieprawidłowa ścieżka programu.".to_string())?
            .join(if helper { "../../.." } else { "../Frameworks" })
            .join(cef::sys::FRAMEWORK_PATH);
        if bundled_framework.exists() {
            let loader = cef::library_loader::LibraryLoader::new(&executable, helper);
            if !loader.load() {
                return Err("Nie znaleziono biblioteki CEF w paczce aplikacji.".to_string());
            }
            Box::leak(Box::new(loader));
        } else {
            let cef_dir = cef::sys::get_cef_dir()
                .ok_or_else(|| "Nie znaleziono plików CEF dla tej architektury.".to_string())?;
            let path = cef_dir.join(cef::sys::FRAMEWORK_PATH);
            let path = CString::new(path.as_os_str().as_bytes())
                .map_err(|error| format!("Nieprawidłowa ścieżka biblioteki CEF: {error}"))?;
            if unsafe { cef::sys::cef_load_library(path.as_ptr().cast()) } != 1 {
                return Err("Nie udało się załadować biblioteki CEF.".to_string());
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = helper;
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    Ok(())
}

fn configure_tor_proxy(context: &RequestContext) -> Result<(), String> {
    let mut proxy = dictionary_value_create()
        .ok_or_else(|| "CEF nie utworzył ustawień proxy Tor.".to_string())?;
    let mode = CefString::from("mode");
    let mode_value = CefString::from("fixed_servers");
    let server = CefString::from("server");
    let server_value = CefString::from("socks5://127.0.0.1:9050");
    proxy.set_string(Some(&mode), Some(&mode_value));
    proxy.set_string(Some(&server), Some(&server_value));

    let mut value = value_create()
        .ok_or_else(|| "CEF nie utworzył wartości ustawień proxy Tor.".to_string())?;
    value.set_dictionary(Some(&mut proxy));
    let name = CefString::from("proxy");
    let mut error = CefString::from("");
    if context.set_preference(Some(&name), Some(&mut value), Some(&mut error)) == 0 {
        let detail = error.to_string();
        return Err(if detail.is_empty() {
            "CEF nie przyjął ustawień proxy Tor.".to_string()
        } else {
            format!("CEF nie przyjął ustawień proxy Tor: {detail}")
        });
    }
    Ok(())
}

fn parent_handle(window: &Window) -> Result<ParentHandle, String> {
    #[cfg(target_os = "macos")]
    {
        return Ok(ParentHandle(
            window.ns_view().map_err(|error| error.to_string())? as _,
        ));
    }
    #[cfg(target_os = "windows")]
    {
        return Ok(ParentHandle(
            window.hwnd().map_err(|error| error.to_string())? as _,
        ));
    }
    #[cfg(target_os = "linux")]
    {
        let handle = window.window_handle().map_err(|error| error.to_string())?;
        if let RawWindowHandle::Xlib(handle) = handle.as_raw() {
            return Ok(ParentHandle(handle.window as _));
        }
        return Err("CEF wymaga okna X11 na Linuksie.".to_string());
    }
    #[allow(unreachable_code)]
    Err("Ten system nie ma obsługi okna CEF.".to_string())
}

pub(crate) fn content_view_size(window: &Window) -> Result<(f64, f64), String> {
    // ponytail: tauri's inner_size() on macOS returns the *first webview's frame*
    // (tauri-runtime-wry inner_size() when has_children == false) — after we shrink
    // the shell to TOOLBAR_HEIGHT it reports 168, so read the content view instead.
    #[cfg(target_os = "macos")]
    {
        unsafe {
            use objc2::msg_send;
            use objc2_foundation::NSRect;
            let view = window.ns_view().map_err(|error| error.to_string())?
                as *mut objc2::runtime::AnyObject;
            if view.is_null() {
                return Err("Okno nie ma widoku treści.".to_string());
            }
            let frame: NSRect = msg_send![view, frame];
            return Ok((frame.size.width, frame.size.height));
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let scale = window.scale_factor().map_err(|error| error.to_string())?;
        let size = window
            .inner_size()
            .map_err(|error| error.to_string())?
            .to_logical::<f64>(scale);
        return Ok((size.width, size.height));
    }
    #[allow(unreachable_code)]
    Err("Ten system nie ma obsługi okna CEF.".to_string())
}

fn browser_bounds(window: &Window) -> Result<Bounds, String> {
    let (width, height) = content_view_size(window)?;
    let toolbar = if cfg!(target_os = "macos") {
        168.0
    } else {
        136.0
    };
    Ok(Bounds {
        x: 0,
        y: toolbar as i32,
        width: width as i32,
        height: (height - toolbar).max(160.0) as i32,
    })
}

fn set_native_visible(handle: cef::sys::cef_window_handle_t, visible: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    unsafe {
        use objc2::{msg_send, runtime::Bool};
        let view = handle as *mut objc2::runtime::AnyObject;
        if view.is_null() {
            return Err("CEF zwrócił pusty widok.".to_string());
        }
        let _: () = msg_send![view, setHidden: Bool::from(!visible)];
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_SHOW};
        ShowWindow(handle as _, if visible { SW_SHOW } else { SW_HIDE });
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    unsafe {
        use x11_dl::xlib::Xlib;
        let xlib = Xlib::open().map_err(|error| error.to_string())?;
        let display = get_xdisplay();
        if display.is_null() {
            return Err("CEF nie udostępnił wyświetlacza X11.".to_string());
        }
        if visible {
            (xlib.XMapWindow)(display as _, handle as _);
        } else {
            (xlib.XUnmapWindow)(display as _, handle as _);
        }
        (xlib.XFlush)(display as _);
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("Ten system nie ma obsługi widoczności CEF.".to_string())
}

fn set_native_bounds(handle: cef::sys::cef_window_handle_t, bounds: &Bounds) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    unsafe {
        use objc2::msg_send;
        let view = handle as *mut objc2::runtime::AnyObject;
        if view.is_null() {
            return Err("CEF zwrócił pusty widok.".to_string());
        }
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let parent: *mut objc2::runtime::AnyObject = msg_send![view, superview];
        if parent.is_null() {
            return Err("CEF nie ma widoku nadrzędnego.".to_string());
        }
        let parent_frame: NSRect = msg_send![parent, frame];
        let parent_is_flipped: bool = msg_send![parent, isFlipped];
        let origin_y = if parent_is_flipped {
            bounds.y as f64
        } else {
            parent_frame.size.height - bounds.y as f64 - bounds.height as f64
        };
        let _: () = msg_send![view, setFrameOrigin: NSPoint { x: bounds.x as _, y: origin_y }];
        let _: () = msg_send![view, setFrameSize: NSSize { width: bounds.width as _, height: bounds.height as _ }];
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_TOP, SWP_NOACTIVATE, SWP_SHOWWINDOW,
        };
        SetWindowPos(
            handle as HWND,
            HWND_TOP,
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    unsafe {
        use x11_dl::xlib::Xlib;
        let xlib = Xlib::open().map_err(|error| error.to_string())?;
        let display = get_xdisplay();
        if display.is_null() {
            return Err("CEF nie udostępnił wyświetlacza X11.".to_string());
        }
        (xlib.XMoveResizeWindow)(
            display as _,
            handle as _,
            bounds.x,
            bounds.y,
            bounds.width as _,
            bounds.height as _,
        );
        (xlib.XFlush)(display as _);
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("Ten system nie ma obsługi rozmiaru CEF.".to_string())
}

wrap_app! {
    struct BrowserApp {
        inner: Arc<Mutex<CefHandler>>,
    }

    impl App {
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(BrowserProcessHandlerImpl::new(self.inner.clone()))
        }
    }
}

wrap_browser_process_handler! {
    struct BrowserProcessHandlerImpl {
        inner: Arc<Mutex<CefHandler>>,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            let app = self
                .inner
                .lock()
                .expect("CEF lock poisoned")
                .app
                .clone();
            let host = CefHost {
                inner: self.inner.clone(),
            };
            let _ = app.emit("browser-cef-ready", ());
            let callback_app = app.clone();
            let _ = app.run_on_main_thread(move || {
                #[cfg(not(feature = "private"))]
                let restored = crate::restore_session(&callback_app, &host);
                #[cfg(feature = "private")]
                let restored = false;
                if !restored {
                    if let Err(error) = crate::create_tab_internal_with_host(
                        &callback_app,
                        &host,
                        HOME_PAGE.to_string(),
                        false,
                        false,
                        None,
                    ) {
                        let _ = callback_app.emit("browser-cef-error", error);
                    }
                }
            });
        }

        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            #[cfg(target_os = "macos")]
            {
                let (app, scheduled) = {
                    let inner = self.inner.lock().expect("CEF lock poisoned");
                    (inner.app.clone(), inner.message_pump_scheduled.clone())
                };
                if CEF_SHUTTING_DOWN.load(Ordering::Acquire) {
                    return;
                }
                if scheduled.swap(true, Ordering::AcqRel) {
                    return;
                }
                let delay = Duration::from_millis(delay_ms.max(0) as u64);
                let dispatch = move || {
                    let scheduled_for_task = scheduled.clone();
                    let result = app.run_on_main_thread(move || {
                        scheduled_for_task.store(false, Ordering::Release);
                        if !CEF_SHUTTING_DOWN.load(Ordering::Acquire) {
                            cef::do_message_loop_work();
                        }
                    });
                    if result.is_err() {
                        scheduled.store(false, Ordering::Release);
                    }
                };
                thread::spawn(move || {
                    thread::sleep(delay);
                    dispatch();
                });
            }
            #[cfg(not(target_os = "macos"))]
            let _ = delay_ms;
        }
    }
}

wrap_client! {
    struct BrowserClient {
        inner: Arc<Mutex<CefHandler>>,
        tab_id: Option<u64>,
    }

    impl Client {
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(DisplayHandlerImpl::new(self.inner.clone()))
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(LifeSpanHandlerImpl::new(self.inner.clone(), self.tab_id))
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(LoadHandlerImpl::new(self.inner.clone()))
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(RequestHandlerImpl::new(self.inner.clone()))
        }

        fn download_handler(&self) -> Option<DownloadHandler> {
            Some(DownloadHandlerImpl::new(self.inner.clone()))
        }
    }
}

wrap_display_handler! {
    struct DisplayHandlerImpl {
        inner: Arc<Mutex<CefHandler>>,
    }

    impl DisplayHandler {
        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            let Some(browser) = browser else { return };
            let title = title.map(CefString::to_string).unwrap_or_default();
            let (app, state, tab_id) = {
                let inner = self.inner.lock().expect("CEF lock poisoned");
                let Some(tab_id) = inner.browser_tabs.get(&browser.identifier()).copied() else {
                    return;
                };
                (inner.app.clone(), inner.state.clone(), tab_id)
            };
            update_title(&app, &state, tab_id, title);
        }
    }
}

wrap_life_span_handler! {
    struct LifeSpanHandlerImpl {
        inner: Arc<Mutex<CefHandler>>,
        tab_id: Option<u64>,
    }

    impl LifeSpanHandler {
        // Returning 0 makes CEF send performClose: to the browser's window.
        // For tab browsers that is our main window, which would kill the whole
        // app on a single tab close; for devtools it UAFs inside CEF. Return 1
        // in both cases and complete the close ourselves by detaching the
        // child view (CEF's documented contract for non-standard close
        // notification).
        fn do_close(&self, browser: Option<&mut Browser>) -> ::std::os::raw::c_int {
            #[cfg(target_os = "macos")]
            if let Some(browser) = browser {
                if let Some(host) = browser.host() {
                    unsafe {
                        use objc2::{msg_send, runtime::AnyObject};
                        let view = host.window_handle() as *mut AnyObject;
                        if !view.is_null() {
                            let superview: *mut AnyObject = msg_send![view, superview];
                            if !superview.is_null() {
                                let _: () = msg_send![view, removeFromSuperview];
                            }
                        }
                    }
                }
            }
            1
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let Some(browser) = browser else { return };
            let browser = browser.clone();
            let browser_id = browser.identifier();
            let Some(tab_id) = self.tab_id else { return };
            let app = {
                let mut inner = self.inner.lock().expect("CEF lock poisoned");
                inner.browser_tabs.insert(browser_id, tab_id);
                inner.browsers.insert(tab_id, browser);
                inner.app.clone()
            };
            let _ = app.emit("browser-cef-ready", serde_json::json!({ "tab_id": tab_id }));
            let callback_app = app.clone();
            let _ = app.run_on_main_thread(move || {
                let _ = crate::layout_webviews(&callback_app);
            });
            let delayed_app = app.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(250));
                let callback_app = delayed_app.clone();
                let _ = delayed_app.run_on_main_thread(move || {
                    let _ = crate::layout_webviews(&callback_app);
                });
            });
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            let Some(browser) = browser else { return };
            let browser_id = browser.identifier();
            let app = {
                let mut inner = self.inner.lock().expect("CEF lock poisoned");
                if let Some(tab_id) = inner.browser_tabs.remove(&browser_id) {
                    inner.browsers.remove(&tab_id);
                }
                inner.app.clone()
            };
            let callback_app = app.clone();
            let _ = app.run_on_main_thread(move || {
                let _ = crate::layout_webviews(&callback_app);
            });
        }

        fn on_before_popup(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: i32,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: i32,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut i32>,
        ) -> i32 {
            let Some(browser) = browser else { return 1 };
            let (app, state, tab_id) = {
                let inner = self.inner.lock().expect("CEF lock poisoned");
                let Some(tab_id) = inner.browser_tabs.get(&browser.identifier()).copied() else {
                    return 1;
                };
                (inner.app.clone(), inner.state.clone(), tab_id)
            };
            let (private_mode, tor_mode) = state
                .inner
                .lock()
                .expect("browser state lock poisoned")
                .tabs
                .get(&tab_id)
                .map(|tab| (tab.private_mode, tab.tor_mode))
                .unwrap_or((false, false));
            let url = target_url.map(CefString::to_string).unwrap_or_default();
            let _ = app.emit("browser-new-window", serde_json::json!({
                "url": url,
                "tab_id": tab_id,
                "private_mode": private_mode,
                "tor_mode": tor_mode
            }));
            1
        }
    }
}

wrap_load_handler! {
    struct LoadHandlerImpl {
        inner: Arc<Mutex<CefHandler>>,
    }

    impl LoadHandler {
        fn on_load_end(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: i32,
        ) {
            let (Some(browser), Some(frame)) = (browser, frame) else { return };
            if frame.is_main() == 0 {
                return;
            }
            let (app, state, tab_id) = {
                let inner = self.inner.lock().expect("CEF lock poisoned");
                let Some(tab_id) = inner.browser_tabs.get(&browser.identifier()).copied() else {
                    return;
                };
                (inner.app.clone(), inner.state.clone(), tab_id)
            };
            let frame_url = frame.url();
            let url = CefString::from(&frame_url).to_string();
            update_url(&app, &state, tab_id, &url);
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            if frame.map(|value| value.is_main()) == Some(0) {
                return;
            }
            let text = error_text.map(|value| value.to_string()).unwrap_or_default();
            let url = failed_url.map(|value| value.to_string()).unwrap_or_default();
            eprintln!("[cef] load_error code={error_code:?} text={text} url={url}");
        }
    }
}

wrap_request_handler! {
    struct RequestHandlerImpl {
        inner: Arc<Mutex<CefHandler>>,
    }

    impl RequestHandler {
        fn on_before_browse(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: i32,
            _is_redirect: i32,
        ) -> i32 {
            let Some(request) = request else { return 0 };
            let request_url = request.url();
            let url = CefString::from(&request_url).to_string();
            let Ok(parsed) = url::Url::parse(&url) else { return 0 };
            let block_trackers = self
                .inner
                .lock()
                .expect("CEF lock poisoned")
                .state
                .block_trackers
                .load(Ordering::Relaxed);
            let blocked = block_trackers && crate::is_blocked_host(parsed.host_str());
            if blocked { 1 } else { 0 }
        }
    }
}

wrap_download_handler! {
    struct DownloadHandlerImpl {
        inner: Arc<Mutex<CefHandler>>,
    }

    impl DownloadHandler {
        fn on_before_download(
            &self,
            browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            suggested_name: Option<&CefString>,
            callback: Option<&mut BeforeDownloadCallback>,
        ) -> i32 {
            let (Some(browser), Some(callback)) = (browser, callback) else {
                return 0;
            };
            let (app, tab_id) = {
                let inner = self.inner.lock().expect("CEF lock poisoned");
                let Some(tab_id) = inner.browser_tabs.get(&browser.identifier()).copied() else {
                    return 0;
                };
                (inner.app.clone(), tab_id)
            };
            let Ok(download_dir) = app.path().download_dir() else {
                return 0;
            };
            if std::fs::create_dir_all(&download_dir).is_err() {
                return 0;
            }
            let source_url = download_item
                .as_ref()
                .map(|item| {
                    let url = item.original_url();
                    CefString::from(&url).to_string()
                })
                .unwrap_or_default();
            let filename = suggested_name
                .map(CefString::to_string)
                .map(|name| crate::sanitize_download_filename(&name))
                .filter(|name| !name.is_empty())
                .or_else(|| {
                    url::Url::parse(&source_url)
                        .ok()
                        .map(|url| crate::download_filename(&url))
                })
                .unwrap_or_else(|| "download".to_string());
            let destination = crate::unique_download_path(&download_dir, &filename);
            let Some(path) = destination.to_str() else {
                return 0;
            };
            let path = CefString::from(path);
            callback.cont(Some(&path), 0);
            let _ = app.emit(
                "browser-download-started",
                serde_json::json!({ "tab_id": tab_id, "url": source_url }),
            );
            1
        }

        fn on_download_updated(
            &self,
            browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            _callback: Option<&mut DownloadItemCallback>,
        ) {
            let (Some(browser), Some(item)) = (browser, download_item) else {
                return;
            };
            if item.is_complete() == 0 && item.is_canceled() == 0 && item.is_interrupted() == 0 {
                return;
            }
            let (app, tab_id) = {
                let inner = self.inner.lock().expect("CEF lock poisoned");
                let Some(tab_id) = inner.browser_tabs.get(&browser.identifier()).copied() else {
                    return;
                };
                (inner.app.clone(), tab_id)
            };
            let url = item.url();
            let path = item.full_path();
            let _ = app.emit(
                "browser-download-finished",
                serde_json::json!({
                    "tab_id": tab_id,
                    "url": CefString::from(&url).to_string(),
                    "path": CefString::from(&path).to_string(),
                    "success": item.is_complete() != 0
                }),
            );
        }
    }
}

fn update_title(app: &AppHandle, state: &BrowserState, tab_id: u64, title: String) {
    let mut data = state.inner.lock().expect("browser state lock poisoned");
    let Some(tab) = data.tabs.get_mut(&tab_id) else {
        return;
    };
    tab.title = title.clone();
    if let Some(history_id) = tab.last_history_id {
        if let Some(entry) = data.history.iter_mut().find(|entry| entry.id == history_id) {
            entry.title = title.clone();
        }
    }
    drop(data);
    let _ = app.emit(
        "browser-tab-title",
        serde_json::json!({ "tab_id": tab_id, "title": title }),
    );
}

fn update_url(app: &AppHandle, state: &BrowserState, tab_id: u64, url: &str) {
    if url.starts_with("data:text/html") {
        return;
    }
    crate::update_cef_tab_url(app, state, tab_id, url);
}

#[cfg(target_os = "linux")]
fn get_xdisplay() -> *mut std::ffi::c_void {
    cef::get_xdisplay()
}

#[cfg(test)]
mod tests {
    #[test]
    fn start_page_inlines_the_search_form() {
        let page = super::start_page();
        assert!(page.starts_with("data:text/html,"));
        assert!(page.contains("duckduckgo"));
        assert!(page.contains("id%3D%22query%22"));
        assert!(!page.contains("start.css"));
    }
}
