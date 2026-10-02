mod cef_browser;
mod diagnostics;

use std::{
    collections::BTreeMap,
    fs,
    net::{IpAddr, SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use percent_encoding::percent_decode_str;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tauri::{
    menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    AppHandle, Emitter, EventTarget, LogicalPosition, LogicalSize, Manager, RunEvent, State,
};
use url::{form_urlencoded, Host, Url};

use cef_browser::CefHost;

pub fn load_cef_library(helper: bool) -> Result<(), String> {
    cef_browser::load_cef_library(helper)
}

#[cfg(target_os = "macos")]
const TOOLBAR_HEIGHT: f64 = 168.0;
#[cfg(not(target_os = "macos"))]
const TOOLBAR_HEIGHT: f64 = 136.0;
const HOME_PAGE: &str = "nienudno://start";
const STATE_EVENT: &str = "browser-state";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tab {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub private_mode: bool,
    pub tor_mode: bool,
    pub tor_exit_ip: Option<String>,
    pub active: bool,
    #[serde(skip)]
    pub webview_label: String,
    #[serde(skip)]
    pub last_history_id: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub created_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub visited_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserSettings {
    pub language: String,
    pub block_trackers: bool,
}

impl Default for BrowserSettings {
    fn default() -> Self {
        Self {
            language: "pl".to_string(),
            block_trackers: true,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BrowserSnapshot {
    pub tabs: Vec<Tab>,
    pub bookmarks: Vec<Bookmark>,
    pub history: Vec<HistoryEntry>,
    pub settings: BrowserSettings,
}

#[derive(Deserialize)]
struct TorCheck {
    #[serde(rename = "IsTor")]
    is_tor: bool,
    #[serde(rename = "IP")]
    ip: String,
}

impl TorCheck {
    fn exit_ip(self) -> Result<String, String> {
        if !self.is_tor || self.ip.parse::<IpAddr>().is_err() {
            return Err("Serwer SOCKS5 nie zapewnia połączenia przez Tor.".to_string());
        }
        Ok(self.ip)
    }
}

fn executable_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

fn find_tor_binary() -> Option<PathBuf> {
    let path_name = if cfg!(target_os = "windows") {
        "tor.exe"
    } else {
        "tor"
    };
    let mut candidates = vec![executable_in_path(path_name), executable_in_path("tor")];
    #[cfg(target_os = "macos")]
    candidates.extend([
        Some(PathBuf::from("/opt/homebrew/bin/tor")),
        Some(PathBuf::from("/usr/local/bin/tor")),
        Some(PathBuf::from("/usr/bin/tor")),
    ]);
    #[cfg(target_os = "linux")]
    candidates.extend([
        Some(PathBuf::from("/usr/bin/tor")),
        Some(PathBuf::from("/usr/local/bin/tor")),
    ]);
    #[cfg(target_os = "windows")]
    candidates.extend([
        Some(PathBuf::from(r"C:/Program Files/Tor/tor.exe")),
        Some(PathBuf::from(r"C:/Program Files (x86)/Tor/tor.exe")),
    ]);
    candidates.into_iter().flatten().find(|path| path.is_file())
}

fn start_tor(data: &mut BrowserData) -> Result<(), String> {
    let binary = find_tor_binary().ok_or_else(|| {
        "Nie znaleziono programu Tor. Zainstaluj Tor i dodaj program tor do PATH.".to_string()
    })?;
    let tor_data_dir = data.data_dir.join("tor-service");
    fs::create_dir_all(&tor_data_dir).map_err(|error| error.to_string())?;
    let child = Command::new(binary)
        .args([
            "--SocksPort",
            "9050",
            "--DataDirectory",
            tor_data_dir
                .to_str()
                .ok_or_else(|| "Nieprawidłowa ścieżka danych Tor.".to_string())?,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Nie udało się uruchomić Tor: {error}"))?;
    data.tor_process = Some(child);
    Ok(())
}

fn ensure_tor_proxy(state: &BrowserState) -> Result<(), String> {
    let local_proxy = SocketAddr::from(([127, 0, 0, 1], 9050));
    if TcpStream::connect_timeout(&local_proxy, Duration::from_millis(250)).is_ok() {
        return Ok(());
    }

    {
        let mut data = state.inner.lock().expect("browser state lock poisoned");
        if let Some(child) = data.tor_process.as_mut() {
            if child
                .try_wait()
                .map_err(|error| format!("Nie można sprawdzić procesu Tor: {error}"))?
                .is_none()
            {
                return Err("Tor uruchamia się. Spróbuj ponownie za kilka sekund.".to_string());
            }
            data.tor_process = None;
        }
        start_tor(&mut data)?;
    }

    for _ in 0..80 {
        if TcpStream::connect_timeout(&local_proxy, Duration::from_millis(250)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err("Tor nie uruchomił usługi SOCKS5 na 127.0.0.1:9050.".to_string())
}

fn check_tor(state: &BrowserState) -> Result<String, String> {
    ensure_tor_proxy(state)?;
    let proxy =
        reqwest::Proxy::all("socks5h://127.0.0.1:9050").map_err(|error| error.to_string())?;
    let client = reqwest::blocking::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if let Ok(status) = client
            .get("https://check.torproject.org/api/ip")
            .send()
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.json::<TorCheck>())
        {
            return status.exit_ip();
        }
        thread::sleep(Duration::from_secs(2));
    }
    Err("Tor nie zakończył uruchamiania w ciągu 90 sekund.".to_string())
}

struct BrowserData {
    data_dir: PathBuf,
    tabs: BTreeMap<u64, Tab>,
    panel_open: bool,
    next_tab_id: u64,
    next_bookmark_id: u64,
    next_history_id: u64,
    bookmarks: Vec<Bookmark>,
    history: Vec<HistoryEntry>,
    settings: BrowserSettings,
    tor_process: Option<Child>,
    #[cfg(not(feature = "private"))]
    session_dirty: bool,
    #[cfg(not(feature = "private"))]
    last_session_flush: Instant,
}

#[derive(Clone)]
pub struct BrowserState {
    inner: Arc<Mutex<BrowserData>>,
    block_trackers: Arc<AtomicBool>,
    history_writer: Sender<(PathBuf, Vec<HistoryEntry>)>,
}

pub struct CefHostState(pub Mutex<Option<CefHost>>);

fn cef_host(app: &AppHandle) -> Result<CefHost, String> {
    app.state::<CefHostState>()
        .0
        .lock()
        .expect("CEF host lock poisoned")
        .clone()
        .ok_or_else(|| "Silnik CEF nie został uruchomiony.".to_string())
}

impl BrowserState {
    fn load(data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;

        let bookmarks: Vec<Bookmark> = read_json(&data_dir.join("bookmarks.json"));
        let history: Vec<HistoryEntry> = read_json(&data_dir.join("history.json"));
        let settings: BrowserSettings = read_json(&data_dir.join("settings.json"));
        let block_trackers = settings.block_trackers;

        let (history_writer, history_receiver) = mpsc::channel::<(PathBuf, Vec<HistoryEntry>)>();
        thread::spawn(move || {
            while let Ok((path, history)) = history_receiver.recv() {
                let _ = write_json(&path, &history);
            }
        });

        let next_bookmark_id = bookmarks.iter().map(|item| item.id).max().unwrap_or(0) + 1;
        let next_history_id = history.iter().map(|item| item.id).max().unwrap_or(0) + 1;

        Ok(Self {
            inner: Arc::new(Mutex::new(BrowserData {
                data_dir,
                tabs: BTreeMap::new(),
                panel_open: false,
                next_tab_id: 1,
                next_bookmark_id,
                next_history_id,
                bookmarks,
                history,
                settings,
                tor_process: None,
                #[cfg(not(feature = "private"))]
                session_dirty: false,
                #[cfg(not(feature = "private"))]
                last_session_flush: Instant::now(),
            })),
            block_trackers: Arc::new(AtomicBool::new(block_trackers)),
            history_writer,
        })
    }

    fn snapshot(&self) -> BrowserSnapshot {
        let data = self.inner.lock().expect("browser state lock poisoned");
        BrowserSnapshot {
            tabs: data.tabs.values().cloned().collect(),
            bookmarks: data.bookmarks.clone(),
            history: data.history.clone(),
            settings: data.settings.clone(),
        }
    }
}

impl Drop for BrowserData {
    fn drop(&mut self) {
        if let Some(mut child) = self.tor_process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn read_json<T>(path: &Path) -> T
where
    T: DeserializeOwned + Default,
{
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    fs::write(path, json).map_err(|error| error.to_string())
}

fn queue_history_write(state: &BrowserState, data: &BrowserData) {
    let _ = state
        .history_writer
        .send((data.data_dir.join("history.json"), data.history.clone()));
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn should_record_history(
    private_mode: bool,
    current_url: &str,
    next_url: &Url,
    last_history_id: Option<u64>,
) -> bool {
    !private_mode
        && next_url.host_str().is_some()
        && (current_url != next_url.as_str() || last_history_id.is_none())
}

fn emit_state(app: &AppHandle) {
    let state = app.state::<BrowserState>();
    #[cfg(not(feature = "private"))]
    {
        state
            .inner
            .lock()
            .expect("browser state lock poisoned")
            .session_dirty = true;
    }
    let _ = app.emit_to(EventTarget::webview("main"), STATE_EVENT, state.snapshot());
}

#[cfg(not(feature = "private"))]
#[derive(Serialize, Deserialize, Clone)]
struct SessionTab {
    url: String,
    active: bool,
    private_mode: bool,
    tor_mode: bool,
}

#[cfg(not(feature = "private"))]
fn save_session_now(app: &AppHandle) {
    let state = app.state::<BrowserState>();
    let tabs: Vec<SessionTab> = {
        let data = state.inner.lock().expect("browser state lock poisoned");
        data.tabs
            .values()
            .map(|tab| SessionTab {
                url: tab.url.clone(),
                active: tab.active,
                private_mode: tab.private_mode,
                tor_mode: tab.tor_mode,
            })
            .collect()
    };
    if let Ok(data_dir) = app.path().app_data_dir() {
        let _ = write_json(&data_dir.join("session.json"), &tabs);
    }
    diagnostics::log(&format!("session saved: {} tabs", tabs.len()));
}

/// Returns true when at least one tab was restored; false falls back to the
/// default start-page tab in on_context_initialized.
#[cfg(not(feature = "private"))]
pub(crate) fn restore_session(app: &AppHandle, host: &CefHost) -> bool {
    let Ok(data_dir) = app.path().app_data_dir() else {
        return false;
    };
    let mut tabs: Vec<SessionTab> = read_json(&data_dir.join("session.json"));
    if tabs.is_empty() {
        return false;
    }
    // Active tab last so it ends up focused after the batch.
    tabs.sort_by_key(|tab| tab.active);
    let mut restored = 0;
    let mut needs_tor = false;
    for tab in tabs {
        needs_tor |= tab.tor_mode;
        if create_tab_internal_with_host(app, host, tab.url, tab.private_mode, tab.tor_mode, None)
            .is_ok()
        {
            restored += 1;
        }
    }
    if needs_tor {
        // ponytail: background start; a restored Tor page fails closed (SOCKS
        // proxy only) until the user navigates/reloads again.
        let state = app.state::<BrowserState>().inner().clone();
        thread::spawn(move || {
            let _ = ensure_tor_proxy(&state);
        });
    }
    diagnostics::log(&format!("session restored: {restored} tabs"));
    restored > 0
}

fn quit_app(app: &AppHandle) {
    diagnostics::log("quit requested");
    #[cfg(not(feature = "private"))]
    save_session_now(app);
    if app.get_window("main").is_some() {
        if let Ok(host) = cef_host(app) {
            host.close_all_sync();
        }
    }
    app.exit(0);
}

#[cfg(all(feature = "updater", not(feature = "private")))]
fn check_for_update(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        diagnostics::log("update: checking");
        let result: Result<(), Box<dyn std::error::Error>> = async {
            use tauri_plugin_updater::UpdaterExt;
            let update = app.updater()?.check().await?;
            match update {
                Some(update) => {
                    diagnostics::log(&format!("update: {} available, installing", update.version));
                    update.download_and_install(|_, _| {}, || {}).await?;
                    diagnostics::log("update: installed, restarting");
                    app.restart();
                }
                None => diagnostics::log("update: up to date"),
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            diagnostics::log(&format!("update: failed: {error}"));
        }
    });
}

fn search_url(query: &str) -> Result<Url, String> {
    let encoded: String = form_urlencoded::byte_serialize(query.as_bytes()).collect();
    Url::parse(&format!("https://duckduckgo.com/?q={encoded}")).map_err(|error| error.to_string())
}

fn looks_like_address(input: &str) -> bool {
    let host = input.split('/').next().unwrap_or(input);
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok()
        || host.starts_with('[')
        || host.contains('.')
        || host.contains(':')
}

fn normalize_url(input: &str) -> Result<Url, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Url::parse(HOME_PAGE).map_err(|error| error.to_string());
    }
    if trimmed == HOME_PAGE {
        return Url::parse(HOME_PAGE).map_err(|error| error.to_string());
    }

    if let Ok(url) = Url::parse(trimmed) {
        if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() {
            return Ok(url);
        }
    }

    if trimmed.contains(char::is_whitespace) || !looks_like_address(trimmed) {
        return search_url(trimmed);
    }

    Url::parse(&format!("https://{trimmed}")).map_err(|error| error.to_string())
}

#[cfg(test)]
fn is_start_page_url(url: &Url, start_page: &Url) -> bool {
    url.scheme() == start_page.scheme()
        && url.host() == start_page.host()
        && url.port() == start_page.port()
        && url.path() == start_page.path()
        && matches!(url.query(), Some("lang=pl" | "lang=en"))
}

fn is_blocked_host(host: Option<&str>) -> bool {
    const BLOCKED_DOMAINS: &[&str] = &[
        "doubleclick.net",
        "googlesyndication.com",
        "google-analytics.com",
        "connect.facebook.net",
        "facebook.net",
        "ads-twitter.com",
        "adnxs.com",
        "scorecardresearch.com",
        "amazon-adsystem.com",
        "criteo.com",
        "hotjar.com",
    ];

    let Some(host) = host else {
        return false;
    };

    BLOCKED_DOMAINS.iter().any(|domain| {
        host == *domain
            || host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn is_local_destination(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(host)) => {
            host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local")
        }
        _ => true,
    }
}

pub(crate) fn layout_webviews(app: &AppHandle) -> Result<(), String> {
    let host = cef_host(app)?;
    layout_webviews_with_host(app, &host)
}

fn layout_webviews_with_host(app: &AppHandle, host: &CefHost) -> Result<(), String> {
    let window = app
        .get_window("main")
        .ok_or_else(|| "Nie znaleziono głównego okna.".to_string())?;
    let shell = app
        .get_webview("main")
        .ok_or_else(|| "Nie znaleziono paska przeglądarki.".to_string())?;
    // ponytail: window.inner_size() on macOS returns the shell webview's frame
    // (tauri-runtime-wry), so after the first set_size(168) it reports 168 forever —
    // use the content view instead.
    let (content_width, content_height) = cef_browser::content_view_size(&window)?;
    let logical_size: LogicalSize<f64> = LogicalSize::new(content_width, content_height);
    let panel_open = app
        .state::<BrowserState>()
        .inner
        .lock()
        .expect("browser state lock poisoned")
        .panel_open;

    shell
        .set_position(LogicalPosition::new(0.0, 0.0))
        .map_err(|error| error.to_string())?;
    shell
        .set_size(LogicalSize::new(
            logical_size.width,
            if panel_open {
                logical_size.height
            } else {
                TOOLBAR_HEIGHT
            },
        ))
        .map_err(|error| error.to_string())?;

    let (active_id, panel_open) = {
        let state = app.state::<BrowserState>();
        let data = state.inner.lock().expect("browser state lock poisoned");
        (
            data.tabs.values().find(|tab| tab.active).map(|tab| tab.id),
            data.panel_open,
        )
    };
    host.layout(&window, active_id, panel_open)
}

pub(crate) fn update_cef_tab_url(
    app: &AppHandle,
    state: &BrowserState,
    tab_id: u64,
    raw_url: &str,
) {
    let Ok(url) = Url::parse(raw_url) else {
        return;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return;
    }
    let mut data = state.inner.lock().expect("browser state lock poisoned");
    let Some(tab) = data.tabs.get_mut(&tab_id) else {
        return;
    };

    let url_changed = tab.url != url.as_str();
    let record_history =
        should_record_history(tab.private_mode, &tab.url, &url, tab.last_history_id);
    if url_changed {
        tab.title = url.host_str().unwrap_or(url.as_str()).to_string();
        tab.last_history_id = None;
    }
    tab.url = url.to_string();
    if record_history {
        let title = tab.title.clone();
        let history_id = data.next_history_id;
        data.tabs
            .get_mut(&tab_id)
            .expect("tab exists")
            .last_history_id = Some(history_id);
        let entry = HistoryEntry {
            id: history_id,
            title,
            url: url.to_string(),
            visited_at: now_millis(),
        };
        data.next_history_id += 1;
        data.history.push(entry);
        if data.history.len() > 500 {
            let trim_count = data.history.len() - 500;
            data.history.drain(0..trim_count);
        }
        queue_history_write(state, &data);
    }
    drop(data);
    emit_state(app);
}

fn create_tab_internal(
    app: &AppHandle,
    requested_url: String,
    private_mode: bool,
    tor_mode: bool,
    tor_exit_ip: Option<String>,
) -> Result<BrowserSnapshot, String> {
    let host = cef_host(app)?;
    create_tab_internal_with_host(
        app,
        &host,
        requested_url,
        private_mode,
        tor_mode,
        tor_exit_ip,
    )
}

pub(crate) fn create_tab_internal_with_host(
    app: &AppHandle,
    host: &CefHost,
    requested_url: String,
    private_mode: bool,
    tor_mode: bool,
    tor_exit_ip: Option<String>,
) -> Result<BrowserSnapshot, String> {
    let url = normalize_url(&requested_url)?;
    let on_start_page = url.as_str() == HOME_PAGE;
    if tor_mode && !on_start_page && is_local_destination(&url) {
        return Err("Karta Tor nie otwiera adresów lokalnych ani numerycznych IP.".to_string());
    }
    let state = app.state::<BrowserState>();
    let tab_id = {
        let mut data = state.inner.lock().expect("browser state lock poisoned");
        if data.settings.block_trackers && is_blocked_host(url.host_str()) {
            return Err("Ta domena jest zablokowana przez ochronę prywatności.".to_string());
        }
        let tab_id = data.next_tab_id;
        data.next_tab_id += 1;
        tab_id
    };

    let host_window = app
        .get_window("main")
        .ok_or_else(|| "Nie znaleziono głównego okna.".to_string())?;

    {
        let mut data = state.inner.lock().expect("browser state lock poisoned");
        for existing in data.tabs.values_mut() {
            existing.active = false;
        }
        data.tabs.insert(
            tab_id,
            Tab {
                id: tab_id,
                title: if tor_mode {
                    "Tor".to_string()
                } else if private_mode {
                    "Karta prywatna".to_string()
                } else {
                    "Nowa karta".to_string()
                },
                url: url.to_string(),
                private_mode: private_mode || tor_mode,
                tor_mode,
                tor_exit_ip,
                active: true,
                webview_label: format!("cef-tab-{tab_id}"),
                last_history_id: None,
            },
        );
    }

    if let Err(error) =
        host.create_browser(&host_window, tab_id, url.as_str(), private_mode, tor_mode)
    {
        let mut data = state.inner.lock().expect("browser state lock poisoned");
        data.tabs.remove(&tab_id);
        if !data.tabs.values().any(|tab| tab.active) {
            if let Some(tab) = data.tabs.values_mut().next() {
                tab.active = true;
            }
        }
        return Err(error);
    }

    layout_webviews_with_host(app, host)?;
    emit_state(app);
    Ok(state.snapshot())
}

fn download_filename(url: &Url) -> String {
    let candidate = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|value| !value.is_empty())
        .unwrap_or("download");
    let decoded = percent_decode_str(candidate).decode_utf8_lossy();
    sanitize_download_filename(&decoded)
}

fn sanitize_download_filename(filename: &str) -> String {
    let basename = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    let sanitized: String = basename
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() || matches!(sanitized.as_str(), "." | "..") {
        "download".to_string()
    } else {
        sanitized
    }
}

fn unique_download_path(directory: &Path, filename: &str) -> PathBuf {
    let initial = directory.join(filename);
    if !initial.exists() {
        return initial;
    }

    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 1.. {
        let candidate = match extension {
            Some(extension) => directory.join(format!("{stem}-{index}.{extension}")),
            None => directory.join(format!("{stem}-{index}")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

#[tauri::command]
fn get_snapshot(state: State<BrowserState>) -> BrowserSnapshot {
    state.snapshot()
}

#[tauri::command(rename_all = "snake_case")]
fn set_panel_open(app: AppHandle, state: State<BrowserState>, open: bool) -> Result<(), String> {
    state
        .inner
        .lock()
        .expect("browser state lock poisoned")
        .panel_open = open;
    layout_webviews(&app)
}

#[tauri::command(rename_all = "snake_case")]
async fn create_tab(
    app: AppHandle,
    url: Option<String>,
    private_mode: Option<bool>,
    tor_mode: Option<bool>,
) -> Result<BrowserSnapshot, String> {
    let requested_url = url.unwrap_or_else(|| HOME_PAGE.to_string());
    let tor_mode = tor_mode.unwrap_or(false);
    if tor_mode {
        let target = normalize_url(&requested_url)?;
        if target.as_str() != HOME_PAGE && is_local_destination(&target) {
            return Err("Karta Tor nie otwiera adresów lokalnych ani numerycznych IP.".to_string());
        }
    }
    let tor_exit_ip = if tor_mode {
        let state = app.state::<BrowserState>().inner().clone();
        Some(
            tauri::async_runtime::spawn_blocking(move || check_tor(&state))
                .await
                .map_err(|error| format!("Nie udało się sprawdzić Tor: {error}"))??,
        )
    } else {
        None
    };
    create_tab_internal(
        &app,
        requested_url,
        private_mode.unwrap_or(false),
        tor_mode,
        tor_exit_ip,
    )
}

#[tauri::command(rename_all = "snake_case")]
async fn close_tab(app: AppHandle, tab_id: u64) -> Result<BrowserSnapshot, String> {
    close_tab_impl(&app, tab_id)
}

fn close_tab_impl(app: &AppHandle, tab_id: u64) -> Result<BrowserSnapshot, String> {
    let state = app.state::<BrowserState>();
    cef_host(app)?.close(tab_id)?;

    let needs_new_tab = {
        let mut data = state.inner.lock().expect("browser state lock poisoned");
        data.tabs.remove(&tab_id);
        if data.tabs.is_empty() {
            true
        } else {
            if !data.tabs.values().any(|tab| tab.active) {
                if let Some(tab) = data.tabs.values_mut().next() {
                    tab.active = true;
                }
            }
            false
        }
    };

    if needs_new_tab {
        return create_tab_internal(app, HOME_PAGE.to_string(), false, false, None);
    }

    layout_webviews(app)?;
    emit_state(app);
    Ok(state.snapshot())
}

#[tauri::command(rename_all = "snake_case")]
fn activate_tab(
    app: AppHandle,
    state: State<BrowserState>,
    tab_id: u64,
) -> Result<BrowserSnapshot, String> {
    {
        let mut data = state.inner.lock().expect("browser state lock poisoned");
        if !data.tabs.contains_key(&tab_id) {
            return Err("Nie znaleziono karty.".to_string());
        }
        for tab in data.tabs.values_mut() {
            tab.active = tab.id == tab_id;
        }
    }
    layout_webviews(&app)?;
    emit_state(&app);
    Ok(state.snapshot())
}

#[tauri::command(rename_all = "snake_case")]
async fn navigate(
    app: AppHandle,
    state: State<'_, BrowserState>,
    tab_id: u64,
    url: String,
) -> Result<(), String> {
    let target = normalize_url(&url)?;
    let tor_mode = {
        let data = state.inner.lock().expect("browser state lock poisoned");
        let tor_mode = data.tabs.get(&tab_id).is_some_and(|tab| tab.tor_mode);
        if tor_mode && target.as_str() != HOME_PAGE && is_local_destination(&target) {
            return Err("Karta Tor nie otwiera adresów lokalnych ani numerycznych IP.".to_string());
        }
        if data.settings.block_trackers && is_blocked_host(target.host_str()) {
            return Err("Ta domena jest zablokowana przez ochronę prywatności.".to_string());
        }
        tor_mode
    };
    if tor_mode {
        let tor_state = state.inner().clone();
        tauri::async_runtime::spawn_blocking(move || ensure_tor_proxy(&tor_state))
            .await
            .map_err(|error| format!("Nie udało się przygotować Tor: {error}"))??;
    }

    let destination = if target.as_str() == HOME_PAGE {
        HOME_PAGE.to_string()
    } else {
        target.to_string()
    };
    cef_host(&app)?.navigate(tab_id, &destination)
}

#[tauri::command(rename_all = "snake_case")]
fn go_back(app: AppHandle, tab_id: u64) -> Result<(), String> {
    cef_host(&app)?.back(tab_id)
}

#[tauri::command(rename_all = "snake_case")]
fn go_forward(app: AppHandle, tab_id: u64) -> Result<(), String> {
    cef_host(&app)?.forward(tab_id)
}

#[tauri::command(rename_all = "snake_case")]
fn reload(app: AppHandle, tab_id: u64) -> Result<(), String> {
    cef_host(&app)?.reload(tab_id)
}

#[tauri::command]
fn quit_browser(app: AppHandle) {
    quit_app(&app);
}

#[tauri::command]
fn list_history(state: State<BrowserState>) -> Vec<HistoryEntry> {
    state.snapshot().history
}

#[tauri::command]
fn list_bookmarks(state: State<BrowserState>) -> Vec<Bookmark> {
    state.snapshot().bookmarks
}

#[tauri::command(rename_all = "snake_case")]
fn save_bookmark(
    app: AppHandle,
    state: State<BrowserState>,
    title: String,
    url: String,
) -> Result<BrowserSnapshot, String> {
    let target = normalize_url(&url)?;
    let (bookmarks_path, bookmarks) = {
        let mut data = state.inner.lock().expect("browser state lock poisoned");
        if data
            .bookmarks
            .iter()
            .any(|bookmark| bookmark.url == target.as_str())
        {
            drop(data);
            return Ok(state.snapshot());
        }

        let bookmark = Bookmark {
            id: data.next_bookmark_id,
            title: if title.trim().is_empty() {
                target.to_string()
            } else {
                title
            },
            url: target.to_string(),
            created_at: now_millis(),
        };
        data.next_bookmark_id += 1;
        data.bookmarks.push(bookmark);
        (data.data_dir.join("bookmarks.json"), data.bookmarks.clone())
    };
    write_json(&bookmarks_path, &bookmarks)?;
    emit_state(&app);
    Ok(state.snapshot())
}

#[tauri::command(rename_all = "snake_case")]
fn remove_bookmark(
    app: AppHandle,
    state: State<BrowserState>,
    bookmark_id: u64,
) -> Result<BrowserSnapshot, String> {
    let (bookmarks_path, bookmarks) = {
        let mut data = state.inner.lock().expect("browser state lock poisoned");
        data.bookmarks.retain(|bookmark| bookmark.id != bookmark_id);
        (data.data_dir.join("bookmarks.json"), data.bookmarks.clone())
    };
    write_json(&bookmarks_path, &bookmarks)?;
    emit_state(&app);
    Ok(state.snapshot())
}

#[tauri::command]
fn clear_history(app: AppHandle, state: State<BrowserState>) -> Result<BrowserSnapshot, String> {
    let mut data = state.inner.lock().expect("browser state lock poisoned");
    data.history.clear();
    queue_history_write(&state, &data);
    drop(data);
    emit_state(&app);
    Ok(state.snapshot())
}

#[tauri::command]
fn get_settings(state: State<BrowserState>) -> BrowserSettings {
    state.snapshot().settings
}

#[tauri::command]
fn update_settings(
    app: AppHandle,
    state: State<BrowserState>,
    settings: BrowserSettings,
) -> Result<BrowserSnapshot, String> {
    if !matches!(settings.language.as_str(), "pl" | "en") {
        return Err("Nieobsługiwany język.".to_string());
    }

    let (settings_path, settings) = {
        let mut data = state.inner.lock().expect("browser state lock poisoned");
        state
            .block_trackers
            .store(settings.block_trackers, Ordering::Relaxed);
        data.settings = settings;
        (data.data_dir.join("settings.json"), data.settings.clone())
    };
    write_json(&settings_path, &settings)?;
    emit_state(&app);
    Ok(state.snapshot())
}

#[tauri::command(rename_all = "snake_case")]
fn open_devtools(app: AppHandle, tab_id: u64) -> Result<(), String> {
    let window = app
        .get_window("main")
        .ok_or_else(|| "Nie znaleziono głównego okna.".to_string())?;
    cef_host(&app)?.open_devtools(tab_id, &window)
}

// CEF sends isHandlingSendEvent/setHandlingSendEvent: to NSApp (CrAppProtocol), but tao's
// TaoApp implements neither, so CEF dies with an unrecognized-selector NSInvalidArgumentException
// (exit 133 / SIGTRAP), e.g. when closing the window while DevTools is open. Install both
// selectors on NSApplication before tao creates TaoApp. Known cef-rs issue #96.
#[cfg(target_os = "macos")]
fn install_cr_app_protocol() {
    use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};

    static HANDLING_SEND_EVENT: AtomicBool = AtomicBool::new(false);

    extern "C" fn is_handling_send_event(_this: *mut AnyObject, _sel: Sel) -> Bool {
        Bool::from(HANDLING_SEND_EVENT.load(Ordering::SeqCst))
    }

    extern "C" fn set_handling_send_event(_this: *mut AnyObject, _sel: Sel, value: Bool) {
        HANDLING_SEND_EVENT.store(value.as_bool(), Ordering::SeqCst);
    }

    unsafe {
        let cls = objc2::class!(NSApplication) as *const AnyClass as *mut AnyClass;
        let getter: objc2::runtime::Imp = std::mem::transmute::<
            extern "C" fn(*mut AnyObject, Sel) -> Bool,
            _,
        >(is_handling_send_event);
        let setter: objc2::runtime::Imp = std::mem::transmute::<
            extern "C" fn(*mut AnyObject, Sel, Bool),
            _,
        >(set_handling_send_event);
        // Type encodings: BOOL return/arg ("B" on arm64), self ("@"), _cmd (":").
        objc2::ffi::class_addMethod(
            cls,
            objc2::sel!(isHandlingSendEvent),
            getter,
            c"B@:".as_ptr(),
        );
        objc2::ffi::class_addMethod(
            cls,
            objc2::sel!(setHandlingSendEvent:),
            setter,
            c"v@:B".as_ptr(),
        );
    }
}

pub fn run() {
    #[cfg(target_os = "macos")]
    install_cr_app_protocol();
    let builder = tauri::Builder::default();
    #[cfg(all(feature = "updater", not(feature = "private")))]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    let app = builder
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            diagnostics::init(&data_dir);
            let browser_state = BrowserState::load(data_dir).map_err(std::io::Error::other)?;
            let cef_state = browser_state.clone();
            let cef_data_dir = cef_state
                .inner
                .lock()
                .expect("browser state lock poisoned")
                .data_dir
                .clone();
            let resource_dir = app.path().resource_dir().map_err(|error| {
                std::io::Error::other(format!(
                    "Nie udało się ustalić katalogu zasobów aplikacji ({error}). \
                     Aplikacja musi być uruchamiana z kompletnego pakietu .app \
                     (z katalogiem Contents/Resources)."
                ))
            })?;
            app.manage(browser_state);
            app.manage(CefHostState(Mutex::new(None)));

            let app_menu = Submenu::with_items(
                app,
                "NieNudno Browser",
                true,
                &[
                    &PredefinedMenuItem::about(app, Some("O NieNudno Browser"), None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, Some("Ukryj"))?,
                    &PredefinedMenuItem::hide_others(app, Some("Ukryj pozostałe"))?,
                    &PredefinedMenuItem::separator(app)?,
                    // ponytail: PredefinedMenuItem::quit's Cmd+Q never fires on
                    // macOS (action/terminate path dead in muda); custom item
                    // with id routes through on_menu_event like Cmd+T does.
                    &MenuItem::with_id(app, "quit", "Zakończ NieNudno", true, Some("CmdOrCtrl+Q"))?,
                ],
            )?;
            let file_menu = Submenu::with_items(
                app,
                "Plik",
                true,
                &[
                    &MenuItem::with_id(app, "new-tab", "Nowa karta", true, Some("CmdOrCtrl+T"))?,
                    &MenuItem::with_id(
                        app,
                        "close-tab",
                        "Zamknij kartę",
                        true,
                        Some("CmdOrCtrl+W"),
                    )?,
                ],
            )?;
            let edit_menu = Submenu::with_items(
                app,
                "Edycja",
                true,
                &[
                    &PredefinedMenuItem::undo(app, Some("Cofnij"))?,
                    &PredefinedMenuItem::redo(app, Some("Ponów"))?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::cut(app, Some("Wytnij"))?,
                    &PredefinedMenuItem::copy(app, Some("Kopiuj"))?,
                    &PredefinedMenuItem::paste(app, Some("Wklej"))?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::select_all(app, Some("Zaznacz wszystko"))?,
                ],
            )?;
            let reload_item =
                MenuItem::with_id(app, "reload", "Przeładuj stronę", true, Some("CmdOrCtrl+R"))?;
            #[allow(unused_mut)] // show-logs exists in the test build only
            let mut view_items: Vec<&dyn IsMenuItem<tauri::Wry>> = vec![&reload_item];
            #[cfg(not(feature = "private"))]
            let show_logs = MenuItem::with_id(app, "show-logs", "Pokaż logi", true, None::<&str>)?;
            #[cfg(not(feature = "private"))]
            view_items.push(&show_logs);
            let view_menu = Submenu::with_items(app, "Widok", true, &view_items)?;
            #[cfg(all(feature = "updater", not(feature = "private")))]
            let help_menu = Some(Submenu::with_items(
                app,
                "Pomoc",
                true,
                &[&MenuItem::with_id(
                    app,
                    "check-update",
                    "Sprawdź aktualizacje...",
                    true,
                    None::<&str>,
                )?],
            )?);
            #[cfg(not(all(feature = "updater", not(feature = "private"))))]
            let help_menu: Option<Submenu<tauri::Wry>> = None;
            #[allow(unused_mut)] // Pomoc appears when the updater is compiled in
            let mut menubar: Vec<&dyn IsMenuItem<tauri::Wry>> =
                vec![&app_menu, &file_menu, &edit_menu, &view_menu];
            if let Some(help_menu) = &help_menu {
                menubar.push(help_menu);
            }
            app.set_menu(Menu::with_items(app, &menubar)?)?;
            app.on_menu_event(|handle, event| {
                let id = event.id().0.clone();
                diagnostics::log(&format!("menu event: {id}"));
                match id.as_str() {
                    "new-tab" => {
                        if let Err(error) =
                            create_tab_internal(handle, HOME_PAGE.to_string(), false, false, None)
                        {
                            let _ = handle.emit("browser-cef-error", error);
                        }
                    }
                    "close-tab" | "reload" => {
                        let active = handle
                            .state::<BrowserState>()
                            .inner
                            .lock()
                            .expect("browser state lock poisoned")
                            .tabs
                            .values()
                            .find(|tab| tab.active)
                            .map(|tab| tab.id);
                        if let Some(tab_id) = active {
                            let result: Result<(), String> = if id == "close-tab" {
                                close_tab_impl(handle, tab_id).map(|_| ())
                            } else {
                                cef_host(handle).and_then(|host| host.reload(tab_id))
                            };
                            if let Err(error) = result {
                                let _ = handle.emit("browser-cef-error", error);
                            }
                        }
                    }
                    "quit" => quit_app(handle),
                    #[cfg(not(feature = "private"))]
                    "show-logs" => {
                        if let Ok(data_dir) = handle.path().app_data_dir() {
                            let _ = Command::new("open")
                                .arg(diagnostics::logs_dir(&data_dir))
                                .spawn();
                        }
                    }
                    #[cfg(all(feature = "updater", not(feature = "private")))]
                    "check-update" => check_for_update(handle.clone()),
                    _ => {}
                }
            });

            let cef_host = CefHost::initialize(
                app.handle().clone(),
                cef_state.clone(),
                &cef_data_dir,
                &resource_dir,
            )
            .map_err(std::io::Error::other)?;
            *app.state::<CefHostState>()
                .0
                .lock()
                .expect("CEF host lock poisoned") = Some(cef_host);

            let main = app
                .get_window("main")
                .ok_or_else(|| std::io::Error::other("Nie znaleziono głównego okna."))?;
            let app_handle = app.handle().clone();
            main.on_window_event(move |event| {
                match event {
                    tauri::WindowEvent::Resized(_) => {
                        let _ = layout_webviews(&app_handle);
                    }
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        // Test build: save the session and hide instead of
                        // quitting (reopen from the dock; browsers stay alive
                        // so reopen is instant). Private build: plain quit —
                        // close all CEF browsers while the window still exists.
                        #[cfg(feature = "private")]
                        {
                            let _ = api;
                            let host = app_handle
                                .state::<CefHostState>()
                                .0
                                .lock()
                                .expect("CEF host lock poisoned")
                                .clone();
                            if let Some(host) = host {
                                host.close_all_sync();
                            }
                        }
                        #[cfg(not(feature = "private"))]
                        {
                            save_session_now(&app_handle);
                            diagnostics::log("close requested -> hiding window");
                            api.prevent_close();
                            if let Some(window) = app_handle.get_window("main") {
                                let _ = window.hide();
                            }
                        }
                    }
                    _ => {}
                }
            });

            layout_webviews(app.handle()).map_err(std::io::Error::other)?;
            Ok(())
        })
        .invoke_handler(|invoke: tauri::ipc::Invoke| {
            if invoke.message.webview_ref().label() != "main" {
                invoke
                    .resolver
                    .reject("Ta strona nie ma dostępu do poleceń przeglądarki.");
                return true;
            }
            let handler: fn(tauri::ipc::Invoke) -> bool = tauri::generate_handler![
                get_snapshot,
                set_panel_open,
                create_tab,
                close_tab,
                activate_tab,
                navigate,
                go_back,
                go_forward,
                reload,
                list_history,
                list_bookmarks,
                save_bookmark,
                remove_bookmark,
                clear_history,
                get_settings,
                update_settings,
                open_devtools,
                quit_browser
            ];
            handler(invoke)
        })
        .build(tauri::generate_context!())
        .expect("NieNudno Browser nie może wystartować.");
    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if matches!(event, RunEvent::MainEventsCleared) {
            cef::do_message_loop_work();
            #[cfg(not(feature = "private"))]
            {
                let mut flush = false;
                {
                    let state = app.state::<BrowserState>();
                    let mut data = state.inner.lock().expect("browser state lock poisoned");
                    if data.session_dirty
                        && data.last_session_flush.elapsed() >= Duration::from_secs(5)
                    {
                        data.session_dirty = false;
                        data.last_session_flush = Instant::now();
                        flush = true;
                    }
                }
                if flush {
                    save_session_now(app);
                }
            }
        }
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = event {
            if let Some(window) = app.get_window("main") {
                diagnostics::log("reopen -> showing window");
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }
        if matches!(event, RunEvent::Ready) {
            let _ = layout_webviews(app);
        }
        if matches!(
            event,
            RunEvent::WindowEvent {
                ref label,
                event: tauri::WindowEvent::Focused(true),
                ..
            } if label == "main"
        ) {
            let _ = layout_webviews(app);
        }
        if matches!(event, RunEvent::Exit) {
            // Covers quit paths that skipped the close handler (menu quit,
            // dock quit while hidden). Idempotent when browsers are already
            // closed; must run before cef::shutdown().
            if app.get_window("main").is_some() {
                #[cfg(not(feature = "private"))]
                save_session_now(app);
                if let Ok(host) = cef_host(app) {
                    host.close_all_sync();
                }
            }
            cef_browser::prepare_shutdown();
            cef::shutdown();
            diagnostics::log("shutdown complete");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_addresses_and_searches() {
        assert_eq!(normalize_url("").unwrap().as_str(), HOME_PAGE);
        assert_eq!(
            normalize_url("example.com").unwrap().as_str(),
            "https://example.com/"
        );
        assert_eq!(
            normalize_url("rust").unwrap().as_str(),
            "https://duckduckgo.com/?q=rust"
        );
        assert_eq!(
            normalize_url("localhost:8080").unwrap().as_str(),
            "https://localhost:8080/"
        );
        assert!(normalize_url("rust browser")
            .unwrap()
            .as_str()
            .contains("rust+browser"));
    }

    #[test]
    fn only_allows_the_local_start_page() {
        let page = Url::parse("tauri://localhost/start.html?lang=pl").unwrap();
        assert!(is_start_page_url(&page, &page));
        assert!(is_start_page_url(
            &Url::parse("tauri://localhost/start.html?lang=en").unwrap(),
            &page
        ));
        assert!(!is_start_page_url(
            &Url::parse("tauri://localhost/index.html?lang=pl").unwrap(),
            &page
        ));
        assert!(!is_start_page_url(
            &Url::parse("tauri://elsewhere/start.html?lang=pl").unwrap(),
            &page
        ));
    }

    #[test]
    fn blocks_known_tracking_domains_and_allows_other_domains() {
        assert!(is_blocked_host(Some("ads.doubleclick.net")));
        assert!(is_blocked_host(Some("google-analytics.com")));
        assert!(!is_blocked_host(Some("example.com")));
    }

    #[test]
    fn rejects_local_destinations_for_tor_tabs() {
        for address in [
            "http://localhost/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://10.0.0.1/",
            "http://router.local/",
        ] {
            assert!(is_local_destination(&Url::parse(address).unwrap()));
        }
        assert!(!is_local_destination(
            &Url::parse("https://example.com/").unwrap()
        ));
    }

    #[test]
    fn tor_requires_confirmed_exit_address() {
        let status: TorCheck =
            serde_json::from_str(r#"{"IsTor":false,"IP":"198.51.100.10"}"#).unwrap();
        assert!(status.exit_ip().is_err());
        let status: TorCheck =
            serde_json::from_str(r#"{"IsTor":true,"IP":"198.51.100.10"}"#).unwrap();
        assert_eq!(status.exit_ip().unwrap(), "198.51.100.10");
    }

    #[test]
    fn does_not_record_the_same_page_twice() {
        let url = Url::parse("https://example.com/").unwrap();
        assert!(should_record_history(
            false,
            "https://example.com/",
            &url,
            None
        ));
        assert!(!should_record_history(
            false,
            "https://example.com/",
            &url,
            Some(1)
        ));
        assert!(should_record_history(
            false,
            "https://example.com/",
            &Url::parse("https://example.org/").unwrap(),
            Some(1)
        ));
        assert!(!should_record_history(
            true,
            "https://example.com/",
            &url,
            None
        ));
    }

    #[test]
    fn produces_safe_download_names() {
        let url = Url::parse("https://example.com/files/my%20file.zip").unwrap();
        assert_eq!(download_filename(&url), "my_file.zip");
        assert_eq!(
            download_filename(&Url::parse("https://example.com/").unwrap()),
            "download"
        );
        assert_eq!(sanitize_download_filename("../../secret.txt"), "secret.txt");
        assert_eq!(sanitize_download_filename(".."), "download");
    }

    #[test]
    fn never_overwrites_an_existing_download() {
        let directory = std::env::temp_dir().join(format!(
            "nienudno-test-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&directory).unwrap();
        let first = directory.join("report.txt");
        fs::write(&first, "existing").unwrap();

        assert_eq!(
            unique_download_path(&directory, "report.txt"),
            directory.join("report-1.txt")
        );
        assert_eq!(fs::read_to_string(&first).unwrap(), "existing");
        fs::remove_dir_all(directory).unwrap();
    }
}
