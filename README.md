# NieNudno Browser

Szybka, prywatna przeglądarka szkolna zbudowana w Rust, Tauri, CEF i `cef-rs`.

Strony internetowe są renderowane przez Chromium Embedded Framework przez bibliotekę `cef-rs`. Tauri obsługuje okno aplikacji i lokalny pasek. Systemowy WebView służy tylko do paska, nie do wyświetlania stron.

Nowa karta pokazuje prostą stronę startową zapisaną w aplikacji. Nie łączy się ona z żadną stroną internetową. Wyszukiwanie otwiera DuckDuckGo dopiero po wpisaniu zapytania. Strona startowa i pasek używają znaku „N” w SVG. Jego źródło to `src-tauri/icons/icon.svg`; kopia dla interfejsu jest w `ui/mark.svg`. Ikony systemowe powstają z tego pliku za pomocą CLI Tauri. Ustawienia języka i blokowania znanych domen są dostępne w menu.

## Uruchomienie

Wymagane:

- Rust i Cargo,
- Node.js (tylko do uruchomienia CLI Tauri),
- macOS: Xcode Command Line Tools,
- Linux: WebKitGTK 4.1 dla lokalnego paska, zależności Tauri oraz X11 wymagane przez CEF,
- Windows: MSVC Build Tools, Windows SDK i WebView2 Runtime dla lokalnego paska.

Otwórz terminal w katalogu projektu i uruchom:

    npx --yes @tauri-apps/cli@2.11.5 dev

## Budowanie instalatora

Budowanie wykonuje się natywnie na docelowym systemie. Wybierz jedno polecenie:

    npx --yes @tauri-apps/cli@2.11.5 build --bundles dmg
    npx --yes @tauri-apps/cli@2.11.5 build --bundles deb,appimage
    npx --yes @tauri-apps/cli@2.11.5 build --bundles nsis

Gotowe pakiety są w katalogu target/release/bundle. Nie buduj instalatora Windows ani Linux na macOS.

CEF jest dołączony do aplikacji. Pakiet macOS zawiera framework CEF oraz osobny proces pomocniczy CEF. Pakiety Windows i Linux zawierają pliki runtime CEF w zasobach aplikacji. Zwiększa to rozmiar instalatora, ale daje tę samą wersję silnika na każdym uruchomieniu aplikacji.

macOS: otwórz plik .dmg i przeciągnij aplikację do folderu Aplikacje. Pakiet szkolny nie jest podpisany certyfikatem Apple. Jeśli macOS go zablokuje, użyj w Finderze przycisku „Otwórz” z menu kontekstowego aplikacji. Nie wyłączaj ochrony systemu.

Windows: uruchom instalator `.exe`. Tauri może doinstalować WebView2, jeśli go brakuje. CEF jest używany do stron internetowych. Linux: zainstaluj pakiet `.deb` wraz z zależnościami lub uruchom AppImage na obsługiwanym systemie. Pakiety Windows i Linux wymagają osobnego sprawdzenia na tych systemach.

## Prywatność

NieNudno Browser nie wysyła telemetrii. Tryb prywatny używa tymczasowego profilu CEF i nie zapisuje historii aplikacji. Wersja MVP blokuje znane domeny trackerów podczas nawigacji. Nie filtruje wszystkich żądań, reklam ani zasobów stron.

Zwykłe karty zapisują historię lokalnie. Strona startowa nie zapisuje wizyty w historii. NieNudno nie wymaga konta. Logowanie na odwiedzanej stronie może jednak ujawnić tej stronie tożsamość użytkownika.

## Karta Tor

Przycisk **＋ Tor** otwiera prywatną kartę z połączeniem przez Tor. Pasek pod adresem pokazuje, czy Tor jest włączony **dla bieżącej karty**. Karta Tor ma też widoczną etykietę „Tor”. Status pokazuje adres IP węzła wyjściowego sprawdzony przy otwarciu karty przez `check.torproject.org`. To nie jest stały test połączenia. Bez działającego Tor karta nie zostanie otwarta. W karcie Tor historia nie jest zapisywana. Zwykłe karty nie włączają Tor w aplikacji; ustawienia sieciowe systemu mogą jednak zmienić trasę ruchu.

Tor nie jest częścią instalatora. Po kliknięciu **＋ Tor** aplikacja sama uruchomi program `tor`, jeśli znajdzie go w systemie. Usługa SOCKS5 działa lokalnie na `127.0.0.1:9050`:

- macOS 14 lub nowszy: zainstaluj Tor poleceniem `brew install tor`. Przeglądarka uruchomi go sama.
- Linux: zainstaluj pakiet `tor` z repozytorium systemu. Przeglądarka uruchomi program `tor` sama, jeśli jest w `PATH`.
- Windows: pobierz Tor Expert Bundle z [oficjalnej strony Tor](https://www.torproject.org/download/tor/) i zainstaluj lub dodaj `tor.exe` do `PATH`. Przeglądarka uruchomi go sama.

Pozostaw Tor uruchomiony podczas przeglądania. Jeśli program `tor` nie jest znaleziony, zainstaluj go i dodaj jego katalog do `PATH`. Jeśli Tor przestanie działać, karta nie przejdzie na zwykłe połączenie. W karcie Tor dostęp do geolokalizacji i WebRTC jest ograniczony, a nawigacja do adresów lokalnych jest blokowana. Nie jest to pełny Tor Browser: język, strefa czasowa, odcisk przeglądarki i inne dane mogą zdradzić region. Nie można wybrać kraju węzła wyjściowego. Weryfikacja połączenia wysyła żądanie do usługi projektu Tor przez sieć Tor.

Skróty w pasku przeglądarki: Ctrl/Cmd+T nowa karta, Ctrl/Cmd+Shift+T nowa prywatna karta, Ctrl/Cmd+W zamknij kartę, Ctrl/Cmd+L pasek adresu i Ctrl/Cmd+R odśwież stronę. Skróty nie są globalne, jeśli fokus ma strona WWW.

## English

NieNudno Browser is a small school browser built with Rust, Tauri, CEF and `cef-rs`. It has tabs, navigation, bookmarks, history, private tabs, downloads, settings and a basic tracker-domain blocklist.

New tabs open a quiet local start page with no network request. Search results open on DuckDuckGo only after a query is submitted. The start page and toolbar use the app's SVG "N" mark.

The **＋ Tor** button opens a private tab through a locally running Tor SOCKS5 proxy at `127.0.0.1:9050`. The status bar shows whether Tor is enabled for the active tab only. Tor must be installed and started separately. The browser checks the Tor exit IP before opening the tab; the displayed IP is not a continuous connection test. The tab is not opened when Tor cannot be verified.
