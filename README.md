# 🐱 Торрент-зверёк · Torrent Critter

[English below](#english)

Маленький торрент-клиент для Windows с мультяшными котиками: они бегают по полосе прогресса, пока идёт загрузка, и умываются, когда всё скачано.

Написан на Rust: движок — [librqbit](https://github.com/ikatson/rqbit), интерфейс — [egui](https://github.com/emilk/egui). Один exe-файл, без установщика и рантаймов.

## Возможности

- **.torrent и magnet-ссылки** — кнопкой, перетаскиванием файла в окно или двойным кликом по `.torrent` (клиент можно сделать приложением по умолчанию).
- **Котики на полосе прогресса**, три режима:
  - бегущие котики — бегают, прыгают и спотыкаются;
  - клубок дерущихся котиков — растёт вместе с числом пиров, с пылью и новыми котиками, вбегающими внутрь;
  - кошачья стая — по котику на каждого подключённого пира.

  По желанию на раздаче можно включить котика, играющего с клубком.
- **Скачанное не блокируется**: готовый установщик можно запустить, переместить или удалить, не закрывая клиент — раздача продолжается.
- **Русский и английский** интерфейс, переключается в настройках на лету.
- **Трей**: запуск свёрнутым (удобно с автозапуском), сворачивание в трей вместо панели задач, крестик в трей. Повторный запуск или двойной клик по `.torrent` передаёт файл уже работающему клиенту.
- **Загрузки переживают перезапуск**: список и прогресс восстанавливаются, данные дозагружаются (fastresume).
- **Отдельная подпапка** для каждой загрузки, папку можно выбирать при добавлении или запомнить по умолчанию.
- **Поиск пиров**: DHT, TCP и uTP, UPnP для входящего порта, дополнительные открытые трекеры (включая HTTPS — для сетей, где закрыт UDP). Встроенный список можно отключить и добавить в настройках свои трекеры. Private-торренты никаких дополнительных трекеров не получают.
- **Пересчёт хешей** — повторная проверка уже скачанных данных на диске.
- **Окно отладки сети**: DHT, попытки и ошибки соединений, счётчики пиров, трекеры; отчёт копируется в буфер обмена с вырезанными passkey.
- Автозапуск вместе с Windows, сортировка списка, сглаженная скорость и оставшееся время.

## Установка

Скачайте `torrent-client.exe` со страницы [Releases](../../releases) и запустите. Настройки и состояние сессии хранятся в `%APPDATA%\TorrentClient`.

## Сборка из исходников

Нужны [Rust](https://rustup.rs/) и Microsoft C++ Build Tools.

```powershell
cargo build --release
```

Или запустите `build.bat` — он соберёт проект и положит `torrent-client.exe` рядом с исходниками. Тесты: `cargo test`.

## Использование

| Действие | Как |
|---|---|
| Добавить торрент | «Добавить .torrent», перетащить файл в окно или вставить magnet-ссылку |
| Меню загрузки | правый клик по карточке |
| Показать кнопки действий | левый клик по карточке |
| Котики, папка, автозапуск, UPnP, трекеры | «Настройки» |

Удаление «торрента и скачанных файлов» стирает файлы с диска насовсем, не в корзину — клиент переспросит перед этим.

## Ограничения

- Только Windows (реестр для автозапуска и ассоциации `.torrent`, `explorer.exe` для открытия папки).
- Только IPv4.
- Изменение настройки UPnP применяется после перезапуска.

## Ответственность

Клиент — инструмент для протокола BitTorrent. Скачивайте и раздавайте только то, на что у вас есть права.

---

## English

A small BitTorrent client for Windows with cartoon cats: they run along the progress bar while a download is going and sit washing themselves when it's done. Written in Rust on top of [librqbit](https://github.com/ikatson/rqbit) and [egui](https://github.com/emilk/egui) — a single exe, no installer.

**Features**

- `.torrent` files and magnet links: button, drag and drop, or double-click (can be made the default app for `.torrent`).
- Three cat modes for the progress bar (running cats, a tussling ball of cats that grows with the peer count, one cat per connected peer), plus an optional cat playing with yarn while seeding.
- Downloaded files are not locked: run a finished installer, move or delete files while the client keeps seeding.
- Russian and English UI, switchable in Settings.
- Tray: start minimized, minimize to tray, close to tray. Launching again or opening a `.torrent` hands it to the running client.
- Downloads survive restarts (fast resume), one subfolder per download, DHT, TCP and uTP, UPnP, extra open trackers (including HTTPS for networks that block UDP) and your own tracker list. Private torrents never get extra trackers.
- Piece hash recheck and a network debug window with a copyable report (tracker passkeys hidden).

**Install:** download `torrent-client.exe` from [Releases](../../releases). Settings and session state live in `%APPDATA%\TorrentClient`.

**Build:** install [Rust](https://rustup.rs/) and Microsoft C++ Build Tools, then `cargo build --release` (or run `build.bat`). Tests: `cargo test`.

**Limitations:** Windows only, IPv4 only; UPnP changes apply after a restart.

This client is a tool for the BitTorrent protocol. Only download and share content you have the rights to.
