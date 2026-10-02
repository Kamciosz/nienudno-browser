const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const el = (id) => document.getElementById(id);
const HOME_PAGE = "nienudno://start";
const translations = {
  pl: {
    newTab: "Nowa karta", privateTab: "Nowa karta prywatna", privateShort: "Prywatna", privateButton: "＋ Prywatna", torTab: "Nowa karta Tor", torShort: "Tor", torButton: "＋ Tor", torChecking: "Sprawdzam Tor…", torCheckingDetails: "Sprawdzam połączenie przez Tor przed otwarciem karty…", back: "Wstecz", forward: "Dalej",
    reload: "Odśwież", placeholder: "Wpisz adres lub wyszukaj", open: "Otwórz", bookmark: "Dodaj zakładkę",
    menu: "Menu", bookmarks: "Zakładki", history: "Historia", settings: "Ustawienia",
    devtools: "Narzędzia programisty", close: "Zamknij", tabs: "Karty", torOn: "Tor: WŁĄCZONY", torOff: "Tor: WYŁĄCZONY",
    normalDetail: "Ta karta nie używa Tor · historia jest zapisywana", privateDetail: "Ta karta nie używa Tor · bez historii",
    torDetail: "Tylko ta karta · IP sprawdzone przy otwarciu: ", unknownIp: "brak danych", empty: "Lista jest pusta.",
    remove: "Usuń", language: "Język", tracking: "Blokuj znane domeny śledzące",
    save: "Zapisz ustawienia", clear: "Wyczyść historię",
    note: "Ochrona nie blokuje wszystkich reklam.",
    blocked: "Zablokowano nawigację.", downloaded: "Pobrano plik.", failed: "Pobieranie nie powiodło się.", torUnavailable: "Uruchom Tor SOCKS5 na 127.0.0.1:9050.", torStarting: "Tor jeszcze się uruchamia. Spróbuj ponownie za chwilę.", torStartingFailed: "Tor nie uruchomił połączenia w 90 sekund. Spróbuj ponownie.", torLocal: "Karta Tor nie otwiera adresów lokalnych ani numerycznych IP.", torUnverified: "Nie można potwierdzić połączenia przez Tor. Karta nie została otwarta.", httpsInfo: "Połączenie HTTPS", httpInfo: "Strona bez HTTPS"
  },
  en: {
    newTab: "New tab", privateTab: "New private tab", privateShort: "Private", privateButton: "＋ Private", torTab: "New Tor tab", torShort: "Tor", torButton: "＋ Tor", torChecking: "Checking Tor…", torCheckingDetails: "Checking the Tor connection before opening the tab…", back: "Back", forward: "Forward",
    reload: "Reload", placeholder: "Enter an address or search", open: "Go", bookmark: "Bookmark this page",
    menu: "Menu", bookmarks: "Bookmarks", history: "History", settings: "Settings",
    devtools: "Developer tools", close: "Close", tabs: "Tabs", torOn: "Tor: ON", torOff: "Tor: OFF",
    normalDetail: "This tab does not use Tor · history is saved", privateDetail: "This tab does not use Tor · no history",
    torDetail: "This tab only · exit IP checked when opened: ", unknownIp: "unavailable", empty: "Nothing here yet.",
    remove: "Remove", language: "Language", tracking: "Block known tracking domains",
    save: "Save settings", clear: "Clear history",
    note: "Protection does not block every ad.",
    blocked: "Navigation blocked.", downloaded: "Download finished.", failed: "Download failed.", torUnavailable: "Start Tor SOCKS5 on 127.0.0.1:9050.", torStarting: "Tor is still starting. Try again in a moment.", torStartingFailed: "Tor did not connect within 90 seconds. Try again.", torLocal: "Tor tabs cannot open local addresses or numeric IPs.", torUnverified: "The Tor connection could not be verified. The tab was not opened.", httpsInfo: "HTTPS connection", httpInfo: "Page without HTTPS"
  }
};

let snapshot = { tabs: [], bookmarks: [], history: [], settings: { language: "pl", block_trackers: true } };
let panel = null;
let torPending = false;

function currentTab() {
  return snapshot.tabs.find((tab) => tab.active);
}

function t(key) {
  return translations[snapshot.settings.language]?.[key] ?? translations.pl[key];
}

function setStatus(message) {
  el("page-status").textContent = message;
}

function displayAddress(url) {
  return url === HOME_PAGE ? "" : url ?? "";
}

async function action(command, args = {}) {
  try {
    const result = await invoke(command, args);
    if (result?.tabs) {
      snapshot = result;
      render();
    }
    if (command === "navigate" && document.activeElement !== el("address")) {
      el("address").value = displayAddress(currentTab()?.url);
    }
    if (command === "create_tab" || command === "navigate" || command === "update_settings") setStatus("");
    return result;
  } catch (error) {
    const message = String(error);
    if (command === "navigate") el("address").value = displayAddress(currentTab()?.url);
    setStatus(message.includes("zablokowana") ? t("blocked") :
      message.includes("nie zakończył uruchamiania") ? t("torStartingFailed") :
      message.includes("Tor uruchamia się") ? t("torStarting") :
      message.includes("Uruchom Tor") || message.includes("nie uruchomił usługi") ||
      message.includes("Nie znaleziono programu Tor") ? t("torUnavailable") :
      message.includes("Nie można potwierdzić połączenia przez Tor") ? t("torUnverified") :
      message.includes("Karta Tor nie otwiera") ? t("torLocal") : message);
    return null;
  }
}

function render() {
  document.documentElement.lang = snapshot.settings.language;
  for (const [id, key] of [
    ["new-tab", "newTab"], ["new-private", "privateTab"], ["new-tor", "torTab"], ["back", "back"],
    ["forward", "forward"], ["reload", "reload"], ["go", "open"],
    ["bookmark", "bookmark"], ["menu", "menu"], ["close-panel", "close"]
  ]) {
    el(id).title = t(key);
    el(id).setAttribute("aria-label", t(key));
  }
  el("address").placeholder = t("placeholder");
  el("new-private").textContent = t("privateButton");
  el("new-tor").textContent = t(torPending ? "torChecking" : "torButton");
  el("address").setAttribute("aria-label", t("placeholder"));
  el("tabs").setAttribute("aria-label", t("tabs"));
  document.querySelectorAll("[data-i18n]").forEach((node) => {
    node.textContent = t(node.dataset.i18n);
  });
  const active = currentTab();
  const mode = active?.tor_mode ? "tor" : active?.private_mode ? "private" : "normal";
  el("connection-status").className = "status-row " + mode;
  el("connection-mode").textContent = t(active?.tor_mode ? "torOn" : "torOff");
  el("connection-detail").textContent = active?.tor_mode ?
    t("torDetail") + (active.tor_exit_ip ?? t("unknownIp")) :
    t(active?.private_mode ? "privateDetail" : "normalDetail");

  const tabs = el("tabs");
  tabs.replaceChildren();
  for (const tab of snapshot.tabs) {
    const item = document.createElement("div");
    item.className = "tab" + (tab.active ? " active" : "") + (tab.private_mode ? " private" : "") + (tab.tor_mode ? " tor" : "");
    item.setAttribute("role", "tab");
    item.setAttribute("aria-selected", String(tab.active));
    item.tabIndex = 0;
    const title = tab.url === HOME_PAGE ? t("newTab") : tab.title;
    item.setAttribute("aria-label", (tab.tor_mode ? t("torShort") + ": " : tab.private_mode ? t("privateShort") + ": " : "") + title);
    item.addEventListener("click", () => action("activate_tab", { tab_id: tab.id }));
    item.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        action("activate_tab", { tab_id: tab.id });
      }
    });
    if (tab.tor_mode || tab.private_mode) {
      const badge = document.createElement("span");
      badge.className = "tab-mode";
      badge.textContent = t(tab.tor_mode ? "torShort" : "privateShort");
      badge.setAttribute("aria-hidden", "true");
      item.append(badge);
    }
    const label = document.createElement("span");
    label.className = "tab-label";
    label.textContent = title;
    label.title = tab.url;
    const close = document.createElement("button");
    close.className = "tab-close";
    close.type = "button";
    close.textContent = "×";
    close.setAttribute("aria-label", t("close"));
    close.addEventListener("click", (event) => {
      event.stopPropagation();
      action("close_tab", { tab_id: tab.id });
    });
    item.append(label, close);
    tabs.append(item);
  }

  const tab = currentTab();
  if (tab && document.activeElement !== el("address")) {
    el("address").value = displayAddress(tab.url);
  }
  const secure = tab?.url.startsWith("https:");
  const insecure = tab?.url.startsWith("http:");
  el("security-indicator").textContent = secure ? "HTTPS" : insecure ? "HTTP" : "";
  el("security-indicator").title = secure ? t("httpsInfo") : insecure ? t("httpInfo") : "";
  el("security-indicator").classList.toggle("insecure", insecure);
  el("bookmark").textContent = snapshot.bookmarks.some((item) => item.url === tab?.url) ? "★" : "☆";
  el("bookmark").disabled = !tab || tab.url === HOME_PAGE;
  if (panel) renderPanel();
}

async function showPanel(name) {
  panel = name;
  el("menu-panel").classList.add("hidden");
  await action("set_panel_open", { open: true });
  el("overlay").classList.remove("hidden");
  renderPanel();
}

function hidePanel() {
  panel = null;
  el("overlay").classList.add("hidden");
  action("set_panel_open", { open: false });
}

async function toggleMenu() {
  const menu = el("menu-panel");
  const opening = menu.classList.contains("hidden");
  await action("set_panel_open", { open: opening });
  menu.classList.toggle("hidden", !opening);
}

function makeList(items, type) {
  const list = document.createElement("ul");
  list.className = "panel-list";
  if (!items.length) {
    const empty = document.createElement("p");
    empty.className = "notice";
    empty.textContent = t("empty");
    list.append(empty);
    return list;
  }
  for (const item of [...items].reverse()) {
    const row = document.createElement("li");
    const link = document.createElement("button");
    link.type = "button";
    link.textContent = item.title || item.url;
    link.title = item.url;
    link.addEventListener("click", async () => {
      const tab = currentTab();
      if (tab) await action("navigate", { tab_id: tab.id, url: item.url });
      hidePanel();
    });
    row.append(link);
    if (type === "bookmarks") {
      const remove = document.createElement("button");
      remove.type = "button";
      remove.textContent = "×";
      remove.title = t("remove");
      remove.addEventListener("click", () => action("remove_bookmark", { bookmark_id: item.id }));
      row.append(remove);
    }
    list.append(row);
  }
  return list;
}

function renderPanel() {
  const content = el("panel-content");
  content.replaceChildren();
  el("panel-title").textContent = t(panel);
  if (panel === "bookmarks" || panel === "history") {
    content.append(makeList(snapshot[panel], panel));
    if (panel === "history") {
      const clear = document.createElement("button");
      clear.className = "action";
      clear.type = "button";
      clear.textContent = t("clear");
      clear.addEventListener("click", () => action("clear_history"));
      content.append(clear);
    }
    return;
  }
  if (panel === "settings") {
    const form = document.createElement("form");
    form.innerHTML = '<label class="field"><span></span><select name="language"><option value="pl">Polski</option><option value="en">English</option></select></label><label class="field"><input type="checkbox" name="block_trackers"><span></span></label><p class="notice"></p><button class="action" type="submit"></button>';
    const labels = form.querySelectorAll(".field span");
    labels[0].textContent = t("language");
    labels[1].textContent = t("tracking");
    form.querySelector(".notice").textContent = t("note");
    form.querySelector("button").textContent = t("save");
    form.elements.language.value = snapshot.settings.language;
    form.elements.block_trackers.checked = snapshot.settings.block_trackers;
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const result = await action("update_settings", {
        settings: {
          language: form.elements.language.value,
          block_trackers: form.elements.block_trackers.checked
        }
      });
      if (result) hidePanel();
    });
    content.append(form);
  }
}

el("new-tab").addEventListener("click", () => action("create_tab", { url: HOME_PAGE }));
el("new-private").addEventListener("click", () => action("create_tab", { url: HOME_PAGE, private_mode: true }));
el("new-tor").addEventListener("click", async () => {
  if (torPending) return;
  torPending = true;
  el("new-tor").disabled = true;
  el("new-tor").textContent = t("torChecking");
  setStatus(t("torCheckingDetails"));
  try {
    await action("create_tab", { url: HOME_PAGE, tor_mode: true });
  } finally {
    torPending = false;
    el("new-tor").disabled = false;
    el("new-tor").textContent = t("torButton");
  }
});
el("back").addEventListener("click", () => { if (currentTab()) action("go_back", { tab_id: currentTab().id }); });
el("forward").addEventListener("click", () => { if (currentTab()) action("go_forward", { tab_id: currentTab().id }); });
el("reload").addEventListener("click", () => { if (currentTab()) action("reload", { tab_id: currentTab().id }); });
el("address-form").addEventListener("submit", (event) => {
  event.preventDefault();
  if (currentTab()) action("navigate", { tab_id: currentTab().id, url: el("address").value });
  el("address").blur();
});
el("bookmark").addEventListener("click", () => {
  const tab = currentTab();
  if (tab) action("save_bookmark", { title: tab.title, url: tab.url });
});
el("menu").addEventListener("click", toggleMenu);
document.querySelectorAll("[data-panel]").forEach((button) => button.addEventListener("click", () => showPanel(button.dataset.panel)));
el("devtools").addEventListener("click", () => {
  el("menu-panel").classList.add("hidden");
  action("set_panel_open", { open: false });
  if (currentTab()) action("open_devtools", { tab_id: currentTab().id });
});
el("close-panel").addEventListener("click", hidePanel);
el("overlay").addEventListener("click", (event) => { if (event.target === el("overlay")) hidePanel(); });
window.addEventListener("keydown", (event) => {
  const modifier = event.ctrlKey || event.metaKey;
  if (modifier && event.key.toLowerCase() === "t") {
    event.preventDefault();
    action("create_tab", { url: HOME_PAGE, private_mode: event.shiftKey });
  } else if (modifier && event.key.toLowerCase() === "w") {
    event.preventDefault();
    if (currentTab()) action("close_tab", { tab_id: currentTab().id });
  } else if (modifier && event.key.toLowerCase() === "l") {
    event.preventDefault();
    el("address").focus();
    el("address").select();
  } else if (modifier && event.key.toLowerCase() === "r") {
    event.preventDefault();
    if (currentTab()) action("reload", { tab_id: currentTab().id });
  } else if (modifier && event.key.toLowerCase() === "q") {
    event.preventDefault();
    invoke("quit_browser");
  } else if (event.key === "Escape") {
    if (panel) hidePanel();
    el("menu-panel").classList.add("hidden");
    action("set_panel_open", { open: false });
  }
});

async function start() {
  await listen("browser-state", (event) => { snapshot = event.payload; render(); });
  await listen("browser-tab-title", (event) => {
    const { tab_id: tabId, title, history_id: historyId } = event.payload;
    const tab = snapshot.tabs.find((item) => item.id === tabId);
    if (!tab || tab.title === title) return;
    tab.title = title;
    if (historyId != null) {
      const entry = snapshot.history.find((item) => item.id === historyId);
      if (entry) entry.title = title;
    }
    render();
  });
  await listen("browser-new-window", (event) => action("create_tab", {
    url: event.payload.url,
    private_mode: event.payload.private_mode,
    tor_mode: event.payload.tor_mode
  }));
  await listen("browser-blocked-navigation", () => setStatus(t("blocked")));
  await listen("browser-download-finished", (event) => setStatus(t(event.payload.success ? "downloaded" : "failed")));
  const result = await action("get_snapshot");
  if (result) { snapshot = result; render(); }
}

start().catch((error) => setStatus(String(error)));
