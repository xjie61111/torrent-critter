#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, CornerRadius, Stroke};
use librqbit::{AddTorrent, AddTorrentOptions, ListenerMode, ListenerOptions, Session, SessionOptions, SessionPersistenceConfig};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, path::PathBuf, sync::Arc, time::{Duration, Instant}};

mod desktop;
mod i18n;
mod storage;

use i18n::{app_name, t, tf, Language};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    download_dir: String,
    use_default_download_dir: bool,
    show_debug_window: bool,
    cats_by_peers: bool,
    cats_pack_race: bool,
    delete_torrent_after_add: bool,
    autostart: bool,
    add_public_trackers: bool,
    /// User-supplied trackers, added to every non-private torrent.
    custom_trackers: Vec<String>,
    enable_upnp_port_forwarding: bool,
    language: Language,
    /// Cat playing with a ball of yarn on finished (seeding) downloads.
    yarn_when_seeding: bool,
    start_in_tray: bool,
    minimize_to_tray: bool,
    close_to_tray: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let download_dir = std::env::var_os("USERPROFILE")
            .map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
            .join("Downloads").to_string_lossy().into_owned();
        Self { download_dir, use_default_download_dir: false, show_debug_window: false, cats_by_peers: false, cats_pack_race: false, delete_torrent_after_add: true, autostart: false, add_public_trackers: true, custom_trackers: Vec::new(), enable_upnp_port_forwarding: true, language: Language::system_default(), yarn_when_seeding: false, start_in_tray: false, minimize_to_tray: false, close_to_tray: false }
    }
}

fn settings_path() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
        .join("TorrentClient").join("settings.json")
}

fn app_data_dir() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
        .join("TorrentClient")
}

fn public_fallback_trackers() -> Vec<String> {
    vec![
        "udp://tracker.opentrackr.org:1337/announce".into(),
        "udp://open.stealth.si:80/announce".into(),
        "udp://tracker.torrent.eu.org:451/announce".into(),
        "http://tracker.dler.org:6969/announce".into(),
        "http://tracker.renfei.net:8080/announce".into(),
        "https://tracker.aburaya.live:443/announce".into(),
        "https://track3r.site:443/announce".into(),
    ]
}

/// Built-in public trackers (if enabled) followed by the user's own, without duplicates.
fn fallback_trackers(settings: &Settings) -> Vec<String> {
    let mut trackers = if settings.add_public_trackers { public_fallback_trackers() } else { Vec::new() };
    for tracker in &settings.custom_trackers {
        if !trackers.contains(tracker) { trackers.push(tracker.clone()); }
    }
    trackers
}

/// Parses one tracker URL per line; blank lines and `#` comments are skipped.
fn parse_tracker_list(text: &str) -> Result<Vec<String>, String> {
    let mut trackers = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let url = url::Url::parse(line).map_err(|_| tf!("не похоже на адрес трекера: {line}", "does not look like a tracker address: {line}"))?;
        if !matches!(url.scheme(), "udp" | "http" | "https") || url.host_str().is_none() {
            return Err(tf!("трекер должен начинаться с udp://, http:// или https://: {line}", "a tracker must start with udp://, http:// or https://: {line}"));
        }
        if !trackers.iter().any(|t| t == line) { trackers.push(line.to_owned()); }
    }
    Ok(trackers)
}

fn load_settings() -> Settings {
    fs::read(settings_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn set_autostart(enabled: bool) -> Result<(), String> {
    let key = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    if enabled {
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let value = format!("\"{}\"", executable.display());
        let result = std::process::Command::new("reg.exe")
            .args(["add", key, "/v", "TorrentClient", "/t", "REG_SZ", "/d", &value, "/f"])
            .output().map_err(|e| e.to_string())?;
        if result.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&result.stderr).trim().to_string()) }
    } else {
        // Removing a missing value is already the desired disabled state.
        let _ = std::process::Command::new("reg.exe")
            .args(["delete", key, "/v", "TorrentClient", "/f"])
            .output();
        Ok(())
    }
}

fn register_torrent_association() -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let command = format!("\"{}\" \"%1\"", executable.display());
    let entries = [
        ("HKCU\\Software\\Classes\\.torrent", "TorrentClient.Torrent"),
        ("HKCU\\Software\\Classes\\TorrentClient.Torrent", t("Торрент-зверёк", "Torrent Critter")),
        ("HKCU\\Software\\Classes\\TorrentClient.Torrent\\shell\\open\\command", command.as_str()),
    ];
    for (key, value) in entries {
        let args = ["add", key, "/ve", "/d", value, "/t", "REG_SZ", "/f"];
        let result = std::process::Command::new("reg.exe").args(args).output().map_err(|e| e.to_string())?;
        if !result.status.success() { return Err(String::from_utf8_lossy(&result.stderr).trim().to_string()); }
    }
    let open_with = std::process::Command::new("reg.exe").args([
        "add", "HKCU\\Software\\Classes\\.torrent\\OpenWithProgids", "/v", "TorrentClient.Torrent", "/t", "REG_NONE", "/f",
    ]).output().map_err(|e| e.to_string())?;
    if !open_with.status.success() { return Err(String::from_utf8_lossy(&open_with.stderr).trim().to_string()); }
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", "ms-settings:defaultapps"]).spawn();
    Ok(())
}

fn safe_folder_name(value: &str) -> String {
    let cleaned: String = value.chars().map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_end_matches('.');
    if cleaned.is_empty() { t("Загрузка", "Download").into() } else { cleaned.to_owned() }
}

fn format_bytes(bytes: u64) -> String {
    let units: [&str; 5] = [t("Б", "B"), t("КиБ", "KiB"), t("МиБ", "MiB"), t("ГиБ", "GiB"), t("ТиБ", "TiB")];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { tf!("{bytes} Б", "{bytes} B") } else { format!("{value:.1} {}", units[unit]) }
}

fn format_speed(mib_per_sec: f64) -> String {
    tf!("{mib_per_sec:.1} МиБ/с", "{mib_per_sec:.1} MiB/s")
}

fn format_eta(remaining_bytes: u64, mib_per_sec: f64) -> String {
    if mib_per_sec < 0.05 { return "—".into(); }
    let secs = (remaining_bytes as f64 / (mib_per_sec * 1024.0 * 1024.0)).clamp(0.0, u64::MAX as f64) as u64;
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;
    if days > 0 { tf!("{days} д {hours} ч", "{days} d {hours} h") }
    else if hours > 0 { tf!("{hours} ч {minutes} мин", "{hours} h {minutes} min") }
    else if minutes > 0 { tf!("{minutes} мин {seconds} с", "{minutes} min {seconds} s") }
    else { tf!("{seconds} с", "{seconds} s") }
}

#[derive(Clone)]
struct DownloadRow {
    id: usize,
    name: String,
    progress: f32,
    detail: String,
    active: bool,
    finished: bool,
    paused: bool,
    peers: u32,
    downloaded: String,
    total: String,
    speed: String,
    eta: String,
    raw_speed_mib_s: f64,
    remaining_bytes: u64,
    live_peers: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DownloadSort {
    Added,
    Name,
    Progress,
    Speed,
    Peers,
}

impl DownloadSort {
    fn label(self) -> &'static str {
        match self {
            Self::Added => t("По добавлению", "By date added"),
            Self::Name => t("По названию", "By name"),
            Self::Progress => t("По прогрессу", "By progress"),
            Self::Speed => t("По скорости", "By speed"),
            Self::Peers => t("По пирам", "By peers"),
        }
    }
}

#[derive(Clone, Copy)]
struct CatRunner {
    position: f32,
    speed_px_s: f32,
    retiring: bool,
    coat_index: usize,
}

#[derive(Clone, Copy)]
enum CatIconKind { Add, Settings, Exit, Download, Upload, Files }

fn cat_coats() -> [Color32; 6] {
    [
        Color32::from_rgb(244, 174, 112),
        Color32::from_rgb(246, 231, 201),
        Color32::from_rgb(157, 192, 210),
        Color32::from_rgb(205, 164, 190),
        Color32::from_rgb(47, 45, 52),
        Color32::from_rgb(255, 255, 248),
    ]
}

fn paint_cat_icon(painter: &egui::Painter, rect: egui::Rect, kind: CatIconKind) {
    let coat = match kind {
        CatIconKind::Add => Color32::from_rgb(255, 193, 91),
        CatIconKind::Settings => Color32::from_rgb(157, 192, 210),
        CatIconKind::Exit => Color32::from_rgb(230, 165, 171),
        CatIconKind::Download => Color32::from_rgb(244, 174, 112),
        CatIconKind::Upload => Color32::from_rgb(157, 192, 210),
        CatIconKind::Files => Color32::from_rgb(255, 193, 91),
    };
    let ink = Color32::from_rgb(75, 59, 55);
    let size = rect.width().min(rect.height()).max(1.0);
    let center = egui::pos2(rect.left() + size * 0.48, rect.center().y - size * 0.02);
    let r = size * 0.28;
    let ear_left = [center + egui::vec2(-r * 0.92, -r * 0.28), center + egui::vec2(-r * 0.94, -r * 1.35), center + egui::vec2(-r * 0.08, -r * 0.86)];
    let ear_right = [center + egui::vec2(r * 0.08, -r * 0.86), center + egui::vec2(r * 0.94, -r * 1.35), center + egui::vec2(r * 0.92, -r * 0.28)];
    for ear in [ear_left, ear_right] {
        painter.add(egui::Shape::convex_polygon(ear.to_vec(), coat, Stroke::new(size * 0.055, ink)));
    }
    painter.circle_filled(center, r, coat);
    painter.circle_stroke(center, r, Stroke::new(size * 0.055, ink));
    for x in [-0.36_f32, 0.36] { painter.circle_filled(center + egui::vec2(x * r, -0.02 * r), size * 0.045, ink); }
    painter.circle_filled(center + egui::vec2(0.0, 0.28 * r), size * 0.045, Color32::from_rgb(227, 126, 127));
    painter.line_segment([center + egui::vec2(0.0, 0.32 * r), center + egui::vec2(-0.14 * r, 0.46 * r)], Stroke::new(size * 0.035, ink));
    painter.line_segment([center + egui::vec2(0.0, 0.32 * r), center + egui::vec2(0.14 * r, 0.46 * r)], Stroke::new(size * 0.035, ink));
    for side in [-1.0_f32, 1.0] {
        for (start_y, end_y) in [(0.22_f32, 0.03_f32), (0.34, 0.34), (0.45, 0.62)] {
            let start = center + egui::vec2(side * r * 0.72, r * start_y);
            let end = center + egui::vec2(side * r * 1.48, r * end_y);
            painter.line_segment([start, end], Stroke::new((size * 0.035).max(0.65), ink));
        }
    }

    let badge = rect.right_bottom() - egui::vec2(size * 0.18, size * 0.16);
    let badge_r = size * 0.20;
    match kind {
        CatIconKind::Add => {
            painter.circle_filled(badge, badge_r, Color32::WHITE);
            painter.circle_stroke(badge, badge_r, Stroke::new(size * 0.045, ink));
            painter.line_segment([badge - egui::vec2(badge_r * 0.48, 0.0), badge + egui::vec2(badge_r * 0.48, 0.0)], Stroke::new(size * 0.07, ink));
            painter.line_segment([badge - egui::vec2(0.0, badge_r * 0.48), badge + egui::vec2(0.0, badge_r * 0.48)], Stroke::new(size * 0.07, ink));
        }
        CatIconKind::Files => {
            let page = egui::Rect::from_center_size(badge, egui::vec2(badge_r * 1.18, badge_r * 1.36));
            painter.rect_filled(page, CornerRadius::same(1), Color32::WHITE);
            painter.rect_stroke(page, CornerRadius::same(1), Stroke::new(size * 0.04, ink), egui::StrokeKind::Inside);
            painter.line_segment([page.left_top() + egui::vec2(1.0, page.height() * 0.42), page.right_top() + egui::vec2(-1.0, page.height() * 0.42)], Stroke::new(size * 0.035, ink));
            painter.line_segment([page.left_top() + egui::vec2(1.0, page.height() * 0.68), page.right_top() + egui::vec2(-1.0, page.height() * 0.68)], Stroke::new(size * 0.035, ink));
        }
        CatIconKind::Settings => {
            painter.circle_filled(badge + egui::vec2(-badge_r * 0.2, -badge_r * 0.18), badge_r * 0.28, ink);
            painter.circle_filled(badge + egui::vec2(badge_r * 0.25, -badge_r * 0.18), badge_r * 0.28, ink);
            painter.circle_filled(badge + egui::vec2(0.0, badge_r * 0.25), badge_r * 0.22, ink);
            painter.add(egui::Shape::ellipse_filled(badge + egui::vec2(0.0, badge_r * 0.57), egui::vec2(badge_r * 0.56, badge_r * 0.33), ink));
        }
        CatIconKind::Download | CatIconKind::Upload => {
            let direction = if matches!(kind, CatIconKind::Download) { 1.0 } else { -1.0 };
            let start = badge + egui::vec2(0.0, -direction * badge_r * 0.55);
            let end = badge + egui::vec2(0.0, direction * badge_r * 0.48);
            painter.line_segment([start, end], Stroke::new(size * 0.075, ink));
            painter.line_segment([end, end + egui::vec2(-badge_r * 0.45, -direction * badge_r * 0.42)], Stroke::new(size * 0.075, ink));
            painter.line_segment([end, end + egui::vec2(badge_r * 0.45, -direction * badge_r * 0.42)], Stroke::new(size * 0.075, ink));
        }
        CatIconKind::Exit => {
            let start = badge + egui::vec2(-badge_r * 0.65, 0.0);
            let end = badge + egui::vec2(badge_r * 0.65, 0.0);
            painter.line_segment([start, end], Stroke::new(size * 0.075, ink));
            painter.line_segment([end, end + egui::vec2(-badge_r * 0.45, -badge_r * 0.4)], Stroke::new(size * 0.075, ink));
            painter.line_segment([end, end + egui::vec2(-badge_r * 0.45, badge_r * 0.4)], Stroke::new(size * 0.075, ink));
        }
    }
}

fn draw_cat_icon(ui: &mut egui::Ui, kind: CatIconKind, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if matches!(kind, CatIconKind::Files) {
        ui.painter().circle_filled(rect.center(), size * 0.49, Color32::from_rgb(255, 238, 204));
    }
    paint_cat_icon(ui.painter(), rect, kind);
}

fn cat_menu_button(ui: &mut egui::Ui, label: &str, kind: CatIconKind) -> egui::Response {
    let font = egui::FontId::proportional(13.5);
    let text_color = Color32::from_rgb(75, 59, 55);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, text_color);
    let size = egui::vec2(galley.size().x + 36.0, 28.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let fill = if response.is_pointer_button_down_on() { Color32::from_rgb(242, 146, 97) }
        else if response.hovered() { Color32::from_rgb(255, 191, 125) }
        else { Color32::from_rgb(255, 221, 166) };
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    paint_cat_icon(ui.painter(), egui::Rect::from_min_size(rect.min + egui::vec2(3.0, 3.0), egui::vec2(22.0, 22.0)), kind);
    ui.painter().galley(egui::pos2(rect.left() + 29.0, rect.center().y - galley.size().y * 0.5), galley, text_color);
    response
}

fn draw_stat_chip(ui: &mut egui::Ui, title: &str, value: String, fill: Color32, ink: Color32, icon: CatIconKind) {
    egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(8)).inner_margin(egui::vec2(6.0, 2.0)).show(ui, |ui| {
        ui.horizontal(|ui| {
            draw_cat_icon(ui, icon, 16.0);
            ui.label(egui::RichText::new(title).size(12.0).color(ink));
            ui.label(egui::RichText::new(value).size(13.5).strong().color(ink));
        });
    });
}

fn safe_tracker_display(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else { return value.to_owned(); };
    if !url.username().is_empty() { let _ = url.set_username("<hidden>"); }
    if url.password().is_some() { let _ = url.set_password(Some("<hidden>")); }
    if url.query().is_some() { url.set_query(Some("<query hidden>")); }
    url.to_string()
}

#[derive(Clone)]
enum PendingSource { File(PathBuf), Magnet(String) }

/// Query parameters of a magnet link, percent- and plus-decoded.
fn magnet_params(uri: &str) -> Vec<(String, String)> {
    let query = uri.split_once('?').map(|(_, q)| q).unwrap_or("");
    url::form_urlencoded::parse(query.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

/// What to do once a background `add_torrent` finishes.
enum AddKind {
    New { source_file: Option<PathBuf> },
    Recheck { torrent_bytes: Vec<u8> },
}

struct AddOutcome {
    token: u64,
    label: String,
    kind: AddKind,
    /// (torrent name, torrent id, was already in the list)
    result: Result<(Option<String>, Option<usize>, bool), String>,
}

/// An add that is still resolving metadata / initializing in the background.
struct InFlightAdd {
    token: u64,
    label: String,
    started: Instant,
    task: tokio::task::JoinHandle<()>,
}

struct DebugTorrent {
    title: String,
    lines: Vec<String>,
    error: Option<String>,
    trackers: Vec<String>,
}

struct DebugSnapshot {
    report: String,
    network_lines: Vec<String>,
    torrents: Vec<DebugTorrent>,
}

fn save_settings(settings: &Settings, previous_autostart: bool) -> Result<(), String> {
    let path = settings_path();
    if let Some(parent) = path.parent() { let _ = fs::create_dir_all(parent); }
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(path, bytes).map_err(|e| e.to_string())?;
    if settings.autostart != previous_autostart { set_autostart(settings.autostart)?; }
    Ok(())
}

fn cartoon_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::light();
    visuals.panel_fill = Color32::from_rgb(255, 249, 235);
    visuals.window_fill = Color32::from_rgb(255, 253, 246);
    visuals.extreme_bg_color = Color32::from_rgb(255, 255, 252);
    visuals.faint_bg_color = Color32::from_rgb(255, 239, 215);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(255, 221, 166);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(12);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(255, 191, 125);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(12);
    visuals.widgets.active.bg_fill = Color32::from_rgb(242, 146, 97);
    visuals.widgets.active.corner_radius = CornerRadius::same(12);
    visuals.selection.bg_fill = Color32::from_rgb(104, 192, 174);
    visuals.selection.stroke = Stroke::new(1.5_f32, Color32::from_rgb(38, 113, 101));
    visuals
}

fn draw_mascot(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
    let painter = ui.painter();
    let c = rect.center();
    let s = 0.68_f32;
    let ink = Color32::from_rgb(71, 58, 51);
    painter.add(egui::Shape::convex_polygon(
        vec![c + egui::vec2(-20.0, -13.0) * s, c + egui::vec2(-19.0, -29.0) * s, c + egui::vec2(-5.0, -18.0) * s],
        Color32::from_rgb(255, 193, 91), Stroke::new(1.7_f32, ink),
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![c + egui::vec2(20.0, -13.0) * s, c + egui::vec2(19.0, -29.0) * s, c + egui::vec2(5.0, -18.0) * s],
        Color32::from_rgb(255, 193, 91), Stroke::new(1.7_f32, ink),
    ));
    painter.circle_filled(c, 22.0 * s, Color32::from_rgb(255, 205, 112));
    painter.circle_stroke(c, 22.0 * s, Stroke::new(1.7_f32, ink));
    painter.circle_filled(c + egui::vec2(-8.0, -2.0) * s, 1.8, ink);
    painter.circle_filled(c + egui::vec2(8.0, -2.0) * s, 1.8, ink);
    painter.circle_filled(c + egui::vec2(0.0, 5.0) * s, 2.0, Color32::from_rgb(230, 119, 103));
    painter.add(egui::Shape::line(
        vec![c + egui::vec2(-5.0, 9.0) * s, c + egui::vec2(0.0, 12.0) * s, c + egui::vec2(5.0, 9.0) * s],
        Stroke::new(1.5_f32, ink),
    ));
    for side in [-1.0_f32, 1.0] {
        for (start_y, end_y) in [(3.0_f32, 0.0_f32), (5.0, 5.0), (7.0, 10.0)] {
            painter.line_segment(
                [c + egui::vec2(side * 7.0, start_y) * s, c + egui::vec2(side * 13.0, end_y) * s],
                Stroke::new(1.0_f32, ink),
            );
        }
    }
}

fn draw_whiskers(painter: &egui::Painter, nose: egui::Pos2, scale: f32, ink: Color32) {
    for side in [-1.0_f32, 1.0] {
        for (start_y, end_y) in [(-0.2_f32, -1.7_f32), (0.8, 0.8), (1.8, 3.4)] {
            let start = nose + egui::vec2(side * 4.0 * scale, start_y * scale);
            let end = nose + egui::vec2(side * 8.2 * scale, end_y * scale);
            painter.line_segment([start, end], Stroke::new(0.95 * scale, ink));
        }
    }
}

fn rotate_point(point: egui::Vec2, angle: f32) -> egui::Vec2 {
    let (sin, cos) = angle.sin_cos();
    egui::vec2(point.x * cos - point.y * sin, point.x * sin + point.y * cos)
}

fn draw_sparkle(painter: &egui::Painter, center: egui::Pos2, radius: f32, color: Color32) {
    let stroke = Stroke::new((radius * 0.16).max(1.0), color);
    painter.line_segment([center - egui::vec2(radius, 0.0), center + egui::vec2(radius, 0.0)], stroke);
    painter.line_segment([center - egui::vec2(0.0, radius), center + egui::vec2(0.0, radius)], stroke);
    painter.line_segment([center - egui::vec2(radius * 0.65, radius * 0.65), center + egui::vec2(radius * 0.65, radius * 0.65)], stroke);
    painter.line_segment([center + egui::vec2(-radius * 0.65, radius * 0.65), center + egui::vec2(radius * 0.65, -radius * 0.65)], stroke);
}

fn draw_cat(painter: &egui::Painter, center: egui::Pos2, scale: f32, coat: Color32, cycle: f32, jumping_enabled: bool, tumble_enabled: bool) {
    let dark_coat = u16::from(coat.r()) + u16::from(coat.g()) + u16::from(coat.b()) < 260;
    let ink = if dark_coat { Color32::from_rgb(240, 229, 214) } else { Color32::from_rgb(75, 59, 55) };
    let jumping = if jumping_enabled && cycle < 0.24 { (cycle / 0.24 * std::f32::consts::PI).sin() * 9.0 * scale } else { 0.0 };
    let stumbling = if (0.72..0.91).contains(&cycle) { ((cycle - 0.72) / 0.19 * std::f32::consts::PI).sin() } else { 0.0 };
    let sway = ((cycle * std::f32::consts::TAU).sin()) * 1.2 * scale;
    let tumbling = tumble_enabled && (0.73..0.89).contains(&cycle);
    let tumble_phase = ((cycle - 0.73) / 0.16).clamp(0.0, 1.0);
    let c = center + egui::vec2(sway, -jumping + stumbling * 3.0 * scale);
    let angle = if tumbling { tumble_phase * std::f32::consts::TAU } else { stumbling * 0.42 };

    let tail_points = [egui::vec2(-10.0, 2.0), egui::vec2(-17.0, -3.0), egui::vec2(-20.0, -9.0 + sway)];
    let tail: Vec<_> = tail_points.iter().map(|p| c + rotate_point(*p * scale, angle)).collect();
    painter.add(egui::Shape::line(tail, Stroke::new(3.0 * scale, ink)));

    let body_points = [
        egui::vec2(-13.0, -4.0), egui::vec2(-8.0, -11.0), egui::vec2(5.0, -11.0),
        egui::vec2(13.0, -4.0), egui::vec2(10.0, 5.0), egui::vec2(-10.0, 5.0),
    ].iter().map(|p| c + rotate_point(*p * scale, angle)).collect();
    painter.add(egui::Shape::convex_polygon(body_points, coat, Stroke::new(1.5 * scale, ink)));

    let leg_swing = (cycle * std::f32::consts::TAU).sin() * 3.0 * scale;
    for (x, offset) in [(-7.0, leg_swing), (7.0, -leg_swing)] {
        let start = c + rotate_point(egui::vec2(x * scale, 2.0 * scale), angle);
        let foot = c + rotate_point(egui::vec2((x + offset) * scale, 9.0 * scale), angle);
        painter.line_segment([start, foot], Stroke::new(2.2 * scale, ink));
    }

    let head = c + rotate_point(egui::vec2(10.0 * scale, -7.0 * scale), angle);
    let ear_left = [egui::vec2(-7.0, -3.0), egui::vec2(-8.0, -12.0), egui::vec2(-4.0, -10.0), egui::vec2(1.0, -7.0)];
    let ear_right = [egui::vec2(2.0, -7.0), egui::vec2(7.0, -10.0), egui::vec2(11.0, -12.0), egui::vec2(10.0, -2.0)];
    for ear in [ear_left, ear_right] {
        let points = ear.iter().map(|p| head + rotate_point(*p * scale, angle)).collect();
        painter.add(egui::Shape::convex_polygon(points, coat, Stroke::new(1.2 * scale, ink)));
    }
    painter.circle_filled(head, 8.5 * scale, coat);
    painter.circle_stroke(head, 8.5 * scale, Stroke::new(1.4 * scale, ink));
    for ear in [[egui::vec2(-6.2, -5.0), egui::vec2(-6.7, -10.2), egui::vec2(-2.7, -7.6)], [egui::vec2(4.0, -7.6), egui::vec2(8.0, -10.2), egui::vec2(7.8, -4.8)]] {
        let points = ear.iter().map(|p| head + rotate_point(*p * scale, angle)).collect();
        painter.add(egui::Shape::convex_polygon(points, Color32::from_rgb(238, 150, 145), Stroke::NONE));
    }
    for x in [-2.8, 3.1] {
        let eye = head + rotate_point(egui::vec2(x * scale, -1.4 * scale), angle);
        if stumbling > 0.75 {
            painter.line_segment([eye + egui::vec2(-1.5, 0.0) * scale, eye + egui::vec2(1.5, 0.0) * scale], Stroke::new(1.35 * scale, ink));
        } else {
            painter.circle_filled(eye, 1.15 * scale, ink);
        }
    }
    let nose = head + rotate_point(egui::vec2(0.4 * scale, 1.5 * scale), angle);
    painter.circle_filled(nose, 1.3 * scale, Color32::from_rgb(227, 126, 127));
    painter.line_segment([nose + egui::vec2(0.0, 1.0) * scale, nose + egui::vec2(-1.4, 2.2) * scale], Stroke::new(0.9 * scale, ink));
    painter.line_segment([nose + egui::vec2(0.0, 1.0) * scale, nose + egui::vec2(1.4, 2.2) * scale], Stroke::new(0.9 * scale, ink));
    draw_whiskers(painter, nose, scale, ink);
    if stumbling > 0.55 {
        draw_sparkle(painter, head + egui::vec2(11.0, -12.0) * scale, 4.0 * scale, Color32::from_rgb(242, 158, 75));
    }
}

fn draw_sitting_cat(painter: &egui::Painter, center: egui::Pos2, scale: f32, coat: Color32, time: f32) {
    let dark_coat = u16::from(coat.r()) + u16::from(coat.g()) + u16::from(coat.b()) < 260;
    let ink = if dark_coat { Color32::from_rgb(240, 229, 214) } else { Color32::from_rgb(75, 59, 55) };
    let lick = (time * 1.8).sin() > 0.86;
    let blink_phase = (time * 0.2).fract();
    let blinking = (0.90..0.97).contains(&blink_phase);
    let purr_pulse = (time * 2.4).sin().max(0.0);
    // A tiny rhythmic tremble gives the seated pose a soft purring motion.
    let center = center + egui::vec2((time * 18.0).sin() * purr_pulse * 0.32 * scale, 0.0);
    painter.add(egui::Shape::line(vec![center + egui::vec2(-8.0, 3.0) * scale, center + egui::vec2(-17.0, 0.0) * scale, center + egui::vec2(-19.0, -7.0 + time.sin() * 2.0) * scale], Stroke::new(3.0 * scale, ink)));
    let body = center + egui::vec2(-1.0, 2.0) * scale;
    painter.circle_filled(body, 12.0 * scale, coat);
    painter.circle_stroke(body, 12.0 * scale, Stroke::new(1.4 * scale, ink));
    for x in [-6.0, 6.0] { painter.line_segment([center + egui::vec2(x, 8.0) * scale, center + egui::vec2(x + 2.0, 13.0) * scale], Stroke::new(2.0 * scale, ink)); }
    let head = center + egui::vec2(5.0, -9.0) * scale;
    let flick_phase = (time * 0.22).fract();
    let ear_flick = if (0.52..0.68).contains(&flick_phase) {
        ((flick_phase - 0.52) / 0.16 * std::f32::consts::PI).sin() * 0.16
    } else { 0.0 };
    let ear_pivot = egui::vec2(3.0, -7.0);
    let outer_ears = [
        [egui::vec2(-8.0, -1.0), egui::vec2(-10.0, -16.0), egui::vec2(-1.0, -7.0)],
        [egui::vec2(8.0, -1.0), egui::vec2(10.0, -16.0), egui::vec2(1.0, -7.0)],
    ];
    for (index, ear) in outer_ears.iter().enumerate() {
        let points = ear.iter().map(|point| {
            let point = if index == 1 { ear_pivot + rotate_point(*point - ear_pivot, ear_flick) } else { *point };
            head + point * scale
        }).collect();
        painter.add(egui::Shape::convex_polygon(points, coat, Stroke::new(1.25 * scale, ink)));
    }
    painter.circle_filled(head, 9.0 * scale, coat);
    painter.circle_stroke(head, 9.0 * scale, Stroke::new(1.4 * scale, ink));
    let inner_ears = [
        [egui::vec2(-6.6, -3.0), egui::vec2(-7.5, -12.0), egui::vec2(-2.2, -6.7)],
        [egui::vec2(6.6, -3.0), egui::vec2(7.5, -12.0), egui::vec2(2.2, -6.7)],
    ];
    for (index, ear) in inner_ears.iter().enumerate() {
        let points = ear.iter().map(|point| {
            let point = if index == 1 { ear_pivot + rotate_point(*point - ear_pivot, ear_flick) } else { *point };
            head + point * scale
        }).collect();
        painter.add(egui::Shape::convex_polygon(points, Color32::from_rgb(238, 150, 145), Stroke::NONE));
    }
    for x in [-2.0, 4.0] {
        let eye = head + egui::vec2(x, -1.0) * scale;
        if lick || blinking { painter.line_segment([eye + egui::vec2(-1.8, 0.0) * scale, eye + egui::vec2(1.8, 0.0) * scale], Stroke::new(1.4 * scale, ink)); }
        else { painter.circle_filled(eye, 0.9 * scale, ink); }
    }
    let nose = head + egui::vec2(1.0, 3.0) * scale;
    painter.circle_filled(nose, 1.3 * scale, Color32::from_rgb(227, 126, 127));
    draw_whiskers(painter, nose, scale, ink);
    if lick { painter.line_segment([head + egui::vec2(1.0, 4.0) * scale, head + egui::vec2(2.0, 8.0) * scale], Stroke::new(2.0 * scale, Color32::from_rgb(238, 126, 145))); }
    let paw = if lick { head + egui::vec2(-2.0, 5.0) * scale } else { center + egui::vec2(3.0, 6.0) * scale };
    painter.line_segment([center + egui::vec2(1.0, -1.0) * scale, paw], Stroke::new(2.5 * scale, ink));
    painter.circle_filled(paw, 2.3 * scale, coat);
    painter.circle_stroke(paw, 2.3 * scale, Stroke::new(1.0 * scale, ink));

    // Soft purr marks swell and fade beside the cheeks in time with the body.
    let purr_alpha = (purr_pulse * 150.0) as u8;
    let purr_color = Color32::from_rgba_unmultiplied(126, 145, 125, purr_alpha);
    let wave = (purr_pulse * 1.5 + 1.0) * scale;
    for side in [-1.0_f32, 1.0] {
        for offset in [0.0_f32, 3.5] {
            let x = side * (13.0 + offset) * scale;
            painter.add(egui::Shape::line(vec![
                head + egui::vec2(x, 1.0 * scale),
                head + egui::vec2(x + side * 2.0 * scale, (1.0 - wave) * scale),
                head + egui::vec2(x, (1.0 - 2.0 * wave) * scale),
            ], Stroke::new(1.0 * scale, purr_color)));
        }
    }
}

fn draw_tussle_head(painter: &egui::Painter, center: egui::Pos2, coat: Color32, blink: bool, scale: f32) {
    let dark_coat = u16::from(coat.r()) + u16::from(coat.g()) + u16::from(coat.b()) < 260;
    let ink = if dark_coat { Color32::from_rgb(240, 229, 214) } else { Color32::from_rgb(75, 59, 55) };
    let ears = [
        [egui::vec2(-7.0, -3.0), egui::vec2(-8.0, -13.0), egui::vec2(-3.0, -9.0)],
        [egui::vec2(2.0, -9.0), egui::vec2(8.0, -13.0), egui::vec2(8.0, -2.0)],
    ];
    for ear in ears {
        painter.add(egui::Shape::convex_polygon(ear.iter().map(|p| center + *p * scale).collect(), coat, Stroke::new(1.2 * scale, ink)));
    }
    painter.circle_filled(center, 8.5 * scale, coat);
    painter.circle_stroke(center, 8.5 * scale, Stroke::new(1.2 * scale, ink));
    for x in [-2.8, 3.0] {
        let eye = center + egui::vec2(x, -1.0) * scale;
        if blink { painter.line_segment([eye + egui::vec2(-1.3, 0.0) * scale, eye + egui::vec2(1.3, 0.0) * scale], Stroke::new(1.1 * scale, ink)); }
        else { painter.circle_filled(eye, 1.0 * scale, ink); }
    }
    let nose = center + egui::vec2(0.5, 2.0) * scale;
    painter.circle_filled(nose, 1.2 * scale, Color32::from_rgb(227, 126, 127));
    draw_whiskers(painter, nose, scale, ink);
    painter.line_segment([center + egui::vec2(0.5, 3.0) * scale, center + egui::vec2(-1.0, 4.2) * scale], Stroke::new(0.8 * scale, ink));
    painter.line_segment([center + egui::vec2(0.5, 3.0) * scale, center + egui::vec2(2.0, 4.2) * scale], Stroke::new(0.8 * scale, ink));
}

fn draw_cat_tussle(rect: egui::Rect, painter: &egui::Painter, time: f32, animate: bool, peers: u32, progress: f32) {
    if peers == 0 {
        draw_sitting_cat(painter, egui::pos2(rect.center().x, rect.center().y + 5.0), 1.0, Color32::from_rgb(244, 174, 112), time);
        return;
    }

    let t = if animate { time } else { 0.0 };
    let peer_scale = (peers.min(24) as f32 / 24.0).sqrt();
    let travel = (rect.width() - 88.0).max(0.0);
    let pile_center = egui::pos2(rect.left() + 44.0 + travel * progress.clamp(0.0, 1.0), rect.center().y + 3.0);
    let pile_radius = 11.0 + 18.0 * peer_scale;
    painter.circle_filled(pile_center + egui::vec2(0.0, 5.0), pile_radius, Color32::from_rgba_unmultiplied(236, 219, 190, 105));
    // Soft dust puffs orbit the wrestling pile.
    for i in 0..9 {
        let angle = std::f32::consts::TAU * i as f32 / 9.0 + t * 0.55;
        let radius = pile_radius + 5.0 + ((t * 2.1 + i as f32).sin() + 1.0) * 2.0;
        let puff = pile_center + egui::vec2(angle.cos() * radius, angle.sin() * radius * 0.42 + 7.0);
        painter.circle_filled(puff, 3.5 + ((t + i as f32).cos() + 1.0) * 1.5, Color32::from_rgba_unmultiplied(205, 178, 139, 110));
    }

    let cat_count = (2 + peers as usize / 5).clamp(2, 7);
    let coats = cat_coats();
    for i in 0..cat_count {
        let angle = std::f32::consts::TAU * i as f32 / cat_count as f32 + t * (0.7 + (i % 2) as f32 * 0.3);
        let orbit = pile_radius * 0.38;
        let wobble = (t * 5.0 + i as f32 * 1.7).sin() * 3.0;
        let center = pile_center + egui::vec2(angle.cos() * orbit, angle.sin() * orbit * 0.44 + wobble);
        let cycle = (t * 0.82 + i as f32 * 0.29 + 0.76).fract();
        draw_cat(painter, center, 0.75, coats[i % coats.len()], cycle, true, false);
    }

    // A rolling layer of smoke hides the wrestlers; heads and paws pop out on top.
    for (offset, radius) in [
        (egui::vec2(-13.0, 5.0), pile_radius * 0.65),
        (egui::vec2(7.0, 7.0), pile_radius * 0.74),
        (egui::vec2(18.0, 2.0), pile_radius * 0.48),
    ] {
        painter.circle_filled(pile_center + offset, radius, Color32::from_rgba_unmultiplied(230, 217, 195, 205));
    }

    for i in 0..2 {
        let phase = t * 1.15 + i as f32 * 2.45;
        let emerge = phase.sin();
        if emerge > -0.25 {
            let x = pile_center.x + if i == 0 { -10.0 } else { 10.0 };
            let y = pile_center.y - 11.0 - emerge.max(0.0) * 10.0;
            draw_tussle_head(painter, egui::pos2(x, y), coats[(i + 1) % coats.len()], phase.cos() > 0.88, 0.82);
        }
    }

    for paw_index in 0..5 {
        // Each paw has a different, short pop-out window and a new pseudo-random
        // spot in the cloud every cycle, so paws appear to tussle within the pile.
        let rate = 0.72 + paw_index as f32 * 0.093;
        let phase = t * rate + paw_index as f32 * 1.37;
        let cycle = phase.floor();
        let beat = phase.fract();
        let active = animate && beat < 0.34;
        if !active { continue; }
        let seed = cycle * 12.9898 + paw_index as f32 * 78.233;
        let angle = (seed.sin() * 43758.5453).fract().abs() * std::f32::consts::TAU;
        let radius = pile_radius * (0.12 + (seed.cos() * 24634.6345).fract().abs() * 0.34);
        let anchor = egui::vec2(angle.cos() * radius, angle.sin() * radius * 0.48 + 3.0);
        let side = if (paw_index + cycle as usize) % 2 == 0 { -1.0_f32 } else { 1.0 };
        let sweep = (beat / 0.34 * std::f32::consts::PI).sin();
        let shoulder = pile_center + anchor;
        let paw = shoulder + egui::vec2(side * (4.0 + sweep * 7.0), -sweep * 8.0 + (seed * 0.23).sin() * 2.0);
        let paw_ink = Color32::from_rgb(75, 59, 55);
        painter.line_segment([shoulder, paw], Stroke::new(3.2_f32, paw_ink));
        let coat = coats[paw_index % coats.len()];
        painter.circle_filled(paw, 3.1, coat);
        painter.circle_stroke(paw, 3.1, Stroke::new(1.0_f32, paw_ink));
        if sweep > 0.35 {
            for claw in 0..3 {
                let origin = paw + egui::vec2(side * 1.8, -1.0 + claw as f32 * 1.8);
                painter.line_segment([origin, origin + egui::vec2(side * 4.5, -3.5)], Stroke::new(1.35_f32, paw_ink));
            }
        }

        // Some strikes leave quick scratch marks next to the paw.
        if sweep > 0.72 && (seed * 0.41).sin() > -0.25 {
            let scratch = paw + egui::vec2(side * 5.0, -4.0);
            let slant = if seed.cos() > 0.0 { -1.0 } else { 1.0 };
            for cut in 0..3 {
                let start = scratch + egui::vec2(cut as f32 * 3.0, cut as f32 * 0.8);
                painter.line_segment([
                    start,
                    start + egui::vec2(4.0 * slant, -7.0 + cut as f32 * 0.6),
                ], Stroke::new(1.65_f32, Color32::from_rgb(190, 93, 91)));
            }
        }
    }

    // Cats dash in from the left and disappear into the scuffle.
    if animate {
        for runner in 0..2 {
            let phase = (t * 0.34 + runner as f32 * 0.52).fract();
            let run_end = pile_center.x - pile_radius - 15.0;
            let x = rect.left() + 14.0 + phase * (run_end - rect.left() - 14.0).max(8.0);
            if x < run_end {
                draw_cat(painter, egui::pos2(x, rect.center().y + 8.0), 0.72, coats[runner], (phase * 2.5).fract(), true, false);
            }
        }
    }

    let sparkle_phase = (t * 2.2).fract();
    let sparkle_size = if sparkle_phase < 0.5 { 8.0 } else { 6.0 };
    draw_sparkle(painter, pile_center + egui::vec2(pile_radius + 5.0, -pile_radius * 0.45), sparkle_size, Color32::from_rgb(227, 150, 92));
    draw_sparkle(painter, pile_center + egui::vec2(-pile_radius - 6.0, -pile_radius * 0.2), 6.0, Color32::from_rgb(227, 150, 92));
}

fn draw_cat_herd(rect: egui::Rect, painter: &egui::Painter, time: f32, runners: &[CatRunner]) {
    if runners.is_empty() {
        draw_sitting_cat(painter, egui::pos2(rect.center().x, rect.center().y + 5.0), 0.92, Color32::from_rgb(244, 174, 112), time);
        return;
    }
    let coats = cat_coats();
    let count = runners.len();
    let positions: Vec<_> = runners.iter().map(|cat| rect.left() + cat.position * rect.width()).collect();
    let scale = 1.0_f32;
    for i in 0..count {
        let cycle = (time * 0.22 + i as f32 * 0.173).fract();
        let near_another_cat = positions.iter().enumerate().any(|(j, x)| j != i && (positions[i] - *x).abs() < 34.0);
        let occasional_jump = near_another_cat && i % 3 == 0 && cycle < 0.24;
        let occasional_tumble = i % 7 == 2;
        draw_cat(painter, egui::pos2(positions[i], rect.center().y + 7.0), scale, coats[runners[i].coat_index % coats.len()], cycle, occasional_jump, occasional_tumble);
    }
}

fn draw_cat_yarn_cat(painter: &egui::Painter, center: egui::Pos2, scale: f32, coat: Color32, time: f32, struggle: f32) {
    let kick = (time * (6.0 + struggle * 5.0)).sin();
    let ink = if u16::from(coat.r()) + u16::from(coat.g()) + u16::from(coat.b()) < 260 {
        Color32::from_rgb(240, 229, 214)
    } else {
        Color32::from_rgb(75, 59, 55)
    };
    let body = center + egui::vec2(kick * struggle * 0.7, -1.0 + kick.abs() * struggle * 0.6) * scale;
    let ground_y = center.y + 11.0 * scale;
    // Four separate feet stay down by the track; one front paw reaches out
    // to pat the ball while the other three step and shuffle.
    for index in 0..4 {
        let phase = time * (4.2 + struggle * 4.0) + index as f32 * 1.7;
        let base_x = [-9.0_f32, -4.0, 5.0, 9.0][index];
        let front = index >= 2;
        let batting = front && index == 3;
        let reach = if batting { 10.0 + (phase.sin() * 0.5 + 0.5) * (8.0 + struggle * 5.0) } else { 0.0 };
        let paw_x = base_x + reach;
        let lift = if batting { (phase.sin().max(0.0) * 2.0) + struggle * phase.cos().abs() * 3.0 } else { phase.sin().max(0.0) * (1.6 + struggle * 2.5) };
        let paw = egui::pos2(center.x + paw_x * scale, ground_y - lift * scale);
        let shoulder = body + egui::vec2(base_x * 0.72 * scale, 3.0 * scale);
        painter.line_segment([shoulder, paw], Stroke::new(2.1 * scale, ink));
        painter.add(egui::Shape::ellipse_filled(paw, egui::vec2(2.7 * scale, 1.9 * scale), coat));
        painter.add(egui::Shape::ellipse_stroke(paw, egui::vec2(2.7 * scale, 1.9 * scale), Stroke::new(0.9 * scale, ink)));
        for toe in [-1.0_f32, 0.0, 1.0] {
            painter.line_segment([
                paw + egui::vec2(1.4 * scale, toe * 0.8 * scale),
                paw + egui::vec2(2.3 * scale, toe * 0.8 * scale),
            ], Stroke::new(0.55 * scale, ink));
        }
    }
    painter.add(egui::Shape::ellipse_filled(body, egui::vec2(14.0 * scale, 7.3 * scale), coat));
    painter.add(egui::Shape::ellipse_stroke(body, egui::vec2(14.0 * scale, 7.3 * scale), Stroke::new(1.3 * scale, ink)));
    painter.add(egui::Shape::ellipse_filled(body + egui::vec2(2.0 * scale, 0.2 * scale), egui::vec2(6.0 * scale, 4.2 * scale), Color32::from_rgb(255, 226, 202)));
    let head = center + egui::vec2(12.0, -9.0 + kick * struggle) * scale;
    draw_tussle_head(painter, head, coat, (time * 0.2).fract() > 0.94, scale * 0.9);
    let tail_base = center - egui::vec2(12.0, -2.0) * scale;
    painter.add(egui::Shape::line(vec![
        tail_base,
        tail_base + egui::vec2(-5.0, -5.0 - kick * 1.5) * scale,
        tail_base + egui::vec2(-9.0, 1.0) * scale,
    ], Stroke::new(2.3 * scale, ink)));
}

fn draw_yarn_ball(painter: &egui::Painter, center: egui::Pos2, time: f32) {
    let radius = 8.0;
    let base = Color32::from_rgb(231, 151, 93);
    let ink = Color32::from_rgb(112, 72, 62);
    painter.circle_filled(center, radius, base);
    painter.circle_stroke(center, radius, Stroke::new(1.1_f32, ink));
    let roll = time * 4.0;
    for offset in [-4.0_f32, 0.0, 4.0] {
        let y = offset + roll.sin() * 1.6;
        painter.add(egui::Shape::line(vec![
            center + egui::vec2(-5.0, y + 2.0),
            center + egui::vec2(-1.5, y - 1.0),
            center + egui::vec2(1.5, y + 1.0),
            center + egui::vec2(5.0, y - 2.0),
        ], Stroke::new(1.0_f32, ink)));
    }
}

fn draw_unspooled_thread(rect: egui::Rect, painter: &egui::Painter, start_x: f32, end_x: f32, time: f32, struggle: f32) {
    if end_x <= start_x + 2.0 { return; }
    let mut points = Vec::new();
    let segments = (((end_x - start_x) / 7.0).ceil() as usize).clamp(2, 240);
    for index in 0..=segments {
        let t = index as f32 / segments as f32;
        let x = start_x + (end_x - start_x) * t;
        let wave = (t * 40.0 + time * (1.4 + struggle * 2.0)).sin() * (1.3 + struggle * 1.4);
        points.push(egui::pos2(x, rect.bottom() - 8.0 + wave));
    }
    painter.add(egui::Shape::line(points, Stroke::new(1.4_f32, Color32::from_rgb(157, 105, 78))));
}

fn draw_tangled_thread(painter: &egui::Painter, center: egui::Pos2, scale: f32, time: f32, amount: f32) {
    if amount <= 0.01 { return; }
    let loops = 1.5 + amount * 3.0;
    let segments = (loops * 24.0) as usize;
    let mut spiral = Vec::with_capacity(segments + 1);
    for index in 0..=segments {
        let t = index as f32 / segments as f32;
        let angle = t * loops * std::f32::consts::TAU + time * 2.0;
        let radius = (3.0 + t * 11.0) * scale;
        spiral.push(center + egui::vec2(angle.cos() * radius, angle.sin() * radius * 0.65));
    }
    painter.add(egui::Shape::line(spiral, Stroke::new(1.8_f32, Color32::from_rgb(157, 105, 78))));
    // A couple of loose loops catch the paws too, so the little escape dance reads clearly.
    for paw_side in [-1.0_f32, 1.0] {
        let loop_center = center + egui::vec2(paw_side * 10.0 * scale, 7.0 * scale);
        painter.add(egui::Shape::ellipse_stroke(loop_center, egui::vec2(5.0 * scale, 3.0 * scale), Stroke::new(1.5_f32, Color32::from_rgb(157, 105, 78))));
    }
}

fn draw_cat_yarn_play(rect: egui::Rect, painter: &egui::Painter, time: f32) {
    let scale = (rect.height() / 40.0).clamp(0.85, 1.15);
    let margin = 44.0 * scale;
    let cat_offset = 34.0 * scale;
    let lane = (rect.width() - margin * 2.0 - cat_offset).max(1.0);
    // One full cycle: the ball slowly unspools, the cat gets tangled,
    // then the yarn winds back as the cat frees itself and starts again.
    let roll_speed = 78.0_f32;
    let roll_time = lane / roll_speed;
    let tangle_time = 2.8_f32;
    let rewind_time = (lane / (roll_speed * 1.8)).max(0.8);
    let cycle_time = roll_time + tangle_time + rewind_time;
    let cycle = time.rem_euclid(cycle_time);
    let (ball_x, struggle, tangle_amount) = if cycle < roll_time {
        (rect.left() + margin + cat_offset + roll_speed * cycle, 0.0, 0.0)
    } else if cycle < roll_time + tangle_time {
        let phase = cycle - roll_time;
        let amount = (phase / 0.8).clamp(0.0, 1.0);
        (rect.left() + margin + cat_offset + lane, 0.35 + (phase / tangle_time) * 0.65, amount)
    } else {
        let phase = (cycle - roll_time - tangle_time) / rewind_time;
        (rect.left() + margin + cat_offset + lane * (1.0 - phase), 0.7 * (1.0 - phase), 1.0 - phase)
    };
    let ball_y = rect.bottom() - 8.0 + (time * 3.0).sin() * 1.0;
    let coats = cat_coats();
    let cat_x = (ball_x - cat_offset).clamp(rect.left() + margin, rect.right() - margin);
    let cat_center = egui::pos2(cat_x, rect.bottom() - 17.0 * scale + (time * 5.0).sin() * struggle * 1.2);
    let thread_end = ball_x - 8.0;
    draw_unspooled_thread(rect, painter, rect.left() + margin, thread_end, time, struggle);
    draw_cat_yarn_cat(painter, cat_center, scale, coats[0], time, struggle);
    draw_tangled_thread(painter, cat_center, scale, time, tangle_amount);
    draw_yarn_ball(painter, egui::pos2(ball_x, ball_y), time);
    // Tiny motion ticks emphasize the rolling ball without obscuring progress.
    let tick_alpha = (time * 5.0).sin().abs() * 150.0;
    let tick = Color32::from_rgba_unmultiplied(133, 105, 78, tick_alpha as u8);
    painter.line_segment([egui::pos2(ball_x - 11.0, ball_y - 2.0), egui::pos2(ball_x - 15.0, ball_y - 4.0)], Stroke::new(1.2_f32, tick));
    painter.line_segment([egui::pos2(ball_x + 11.0, ball_y + 2.0), egui::pos2(ball_x + 15.0, ball_y + 4.0)], Stroke::new(1.2_f32, tick));
}

fn draw_cat_progress(ui: &mut egui::Ui, progress: f32, time: f64, animate: bool, is_seeding: bool, peers: u32, live_peers: u32, cats_by_peers: bool, cats_pack_race: bool, yarn_when_seeding: bool, idle_seed: usize, runners: &[CatRunner]) -> egui::Response {
    let width = ui.available_width().max(140.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 46.0), egui::Sense::click());
    let painter = ui.painter().with_clip_rect(rect);
    let radius = CornerRadius::same(14);
    painter.rect_filled(rect, radius, Color32::from_rgb(235, 242, 224));
    let fill_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + rect.width() * progress.clamp(0.0, 1.0), rect.bottom()));
    if fill_rect.width() > 0.0 {
        painter.rect_filled(fill_rect, radius, Color32::from_rgb(196, 231, 198));
    }
    painter.rect_stroke(rect, radius, Stroke::new(1.5_f32, Color32::from_rgb(132, 177, 139)), egui::StrokeKind::Inside);

    let coats = cat_coats();
    let cats = [
        (0.22_f32, 0.98_f32, coats[0]),
        (0.58_f32, 1.08_f32, coats[1]),
        (0.94_f32, 0.92_f32, coats[2]),
        (1.30_f32, 0.96_f32, coats[4]),
        (1.66_f32, 1.00_f32, coats[5]),
    ];
    let speed = if animate { time as f32 } else { 0.0 };
    if is_seeding && yarn_when_seeding {
        draw_cat_yarn_play(rect, &painter, time as f32);
    } else if !animate {
        let count = 1 + idle_seed % 5;
        let spacing = rect.width() / (count + 1) as f32;
        for index in 0..count {
            let cat_index = idle_seed.wrapping_add(index * 3) % cats.len();
            let coat = cats[cat_index].2;
            let x = rect.left() + spacing * (index + 1) as f32;
            draw_sitting_cat(&painter, egui::pos2(x, rect.center().y + 5.0), cats[cat_index].1, coat, time as f32 + index as f32);
        }
    } else if cats_pack_race {
        draw_cat_herd(rect, &painter, time as f32, runners);
    } else if cats_by_peers && peers == 0 {
        let cat_index = idle_seed % cats.len();
        draw_sitting_cat(&painter, egui::pos2(rect.center().x, rect.center().y + 5.0), cats[cat_index].1, cats[cat_index].2, time as f32);
    } else if cats_by_peers {
        draw_cat_tussle(rect, &painter, time as f32, animate, peers, progress);
    } else {
        for (index, (phase, scale, coat)) in cats.into_iter().enumerate() {
            let lane = (rect.width() + 44.0).max(180.0);
            let x = rect.left() - 20.0 + (speed * (53.0 + index as f32 * 13.0) + phase * rect.width()).rem_euclid(lane);
            let cycle = (speed * (0.47 + index as f32 * 0.09) + phase).fract();
            draw_cat(&painter, egui::pos2(x, rect.center().y + 7.0), scale, coat, cycle, true, false);
        }
    }
    painter.text(
        rect.right_bottom() - egui::vec2(8.0, 4.0),
        egui::Align2::RIGHT_BOTTOM,
        if cats_pack_race { tf!("{:.0}% · {live_peers} п.", "{:.0}% · {live_peers} peers", progress.clamp(0.0, 1.0) * 100.0) } else if cats_by_peers { tf!("{:.0}% · {peers} п.", "{:.0}% · {peers} peers", progress.clamp(0.0, 1.0) * 100.0) } else { format!("{:.0}%", progress.clamp(0.0, 1.0) * 100.0) },
        egui::FontId::proportional(13.0),
        Color32::from_rgb(67, 93, 71),
    );
    response
}

struct TorrentClientApp {
    session: Arc<Session>,
    runtime: Option<tokio::runtime::Runtime>,
    settings: Settings,
    saved_autostart: bool,
    saved_upnp_port_forwarding: bool,
    magnet_text: String,
    notice: String,
    selected: Option<usize>,
    show_settings: bool,
    pending_source: Option<PendingSource>,
    pending_folder: String,
    pending_use_default: bool,
    pending_initial_file: Option<PathBuf>,
    debug_copy_notice: String,
    smoothed_download_mib_s: f64,
    smoothed_upload_mib_s: f64,
    smoothed_torrent_speeds: HashMap<usize, f64>,
    last_speed_sample: Option<Instant>,
    cached_rows: Vec<DownloadRow>,
    last_stats_refresh: Option<Instant>,
    cat_herds: HashMap<usize, Vec<CatRunner>>,
    last_cat_frame: Option<Instant>,
    download_sort: DownloadSort,
    add_tx: std::sync::mpsc::Sender<AddOutcome>,
    add_rx: std::sync::mpsc::Receiver<AddOutcome>,
    in_flight: Vec<InFlightAdd>,
    next_add_token: u64,
    confirm_delete: Option<(usize, String)>,
    settings_backup: Option<Settings>,
    trackers_text: String,
    debug_cache: Option<DebugSnapshot>,
    tray: Option<desktop::Tray>,
    applied_language: Language,
    center_settings: bool,
    center_pending: bool,
    hide_at_startup: bool,
}

impl TorrentClientApp {
    fn new(session: Arc<Session>, runtime: tokio::runtime::Runtime, tray: Option<desktop::Tray>) -> Self {
        let settings = load_settings();
        let saved_autostart = settings.autostart;
        let saved_upnp_port_forwarding = settings.enable_upnp_port_forwarding;
        let pending_initial_file = std::env::args_os().nth(1).map(PathBuf::from);
        let (add_tx, add_rx) = std::sync::mpsc::channel();
        // Only with a tray icon: otherwise a hidden window could never be brought back.
        let hide_at_startup = settings.start_in_tray && tray.is_some();
        Self { hide_at_startup, tray, applied_language: settings.language, center_settings: false, center_pending: false, add_tx, add_rx, in_flight: Vec::new(), next_add_token: 0, confirm_delete: None, settings_backup: None, trackers_text: String::new(), debug_cache: None, session, runtime: Some(runtime), settings: settings.clone(), saved_autostart, saved_upnp_port_forwarding, magnet_text: String::new(), notice: t("Готово", "Ready").into(), selected: None, show_settings: false, pending_source: None, pending_folder: settings.download_dir.clone(), pending_use_default: false, pending_initial_file, debug_copy_notice: String::new(), smoothed_download_mib_s: 0.0, smoothed_upload_mib_s: 0.0, smoothed_torrent_speeds: HashMap::new(), last_speed_sample: None, cached_rows: Vec::new(), last_stats_refresh: None, cat_herds: HashMap::new(), last_cat_frame: None, download_sort: DownloadSort::Added }
    }

    fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.as_ref().expect("runtime is only removed during shutdown").block_on(future)
    }

    /// Runs `add_torrent` on the tokio runtime. For magnet links librqbit waits
    /// for metadata inside this call, which can take minutes, so it must never
    /// run on the UI thread. The result comes back through `add_rx`.
    fn spawn_add(&mut self, label: String, source: AddTorrent<'static>, options: AddTorrentOptions, kind: AddKind) {
        let token = self.next_add_token;
        self.next_add_token += 1;
        let session = self.session.clone();
        let tx = self.add_tx.clone();
        let task_label = label.clone();
        let task = self.runtime.as_ref().expect("runtime is only removed during shutdown").spawn(async move {
            let result = session.add_torrent(source, Some(options)).await
                .map(|response| {
                    let already = matches!(response, librqbit::AddTorrentResponse::AlreadyManaged(..));
                    let handle = response.into_handle();
                    (handle.as_ref().and_then(|t| t.name()), handle.map(|t| t.id().into()), already)
                })
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(AddOutcome { token, label: task_label, kind, result });
        });
        self.in_flight.push(InFlightAdd { token, label, started: Instant::now(), task });
    }

    fn cancel_in_flight(&mut self, token: u64) {
        if let Some(index) = self.in_flight.iter().position(|add| add.token == token) {
            let add = self.in_flight.remove(index);
            add.task.abort();
            self.notice = tf!("Добавление отменено: {}", "Adding cancelled: {}", add.label);
        }
    }

    fn process_add_outcomes(&mut self) {
        while let Ok(outcome) = self.add_rx.try_recv() {
            self.in_flight.retain(|add| add.token != outcome.token);
            self.last_stats_refresh = None;
            match (outcome.kind, outcome.result) {
                (AddKind::New { source_file }, Ok((name, _, already))) => {
                    let name = name.unwrap_or(outcome.label);
                    let added = if already { tf!("Уже в списке: {name}", "Already in the list: {name}") } else { tf!("Добавлено: {name}", "Added: {name}") };
                    self.notice = match source_file.filter(|_| self.settings.delete_torrent_after_add) {
                        Some(path) => match fs::remove_file(path) {
                            Ok(()) => tf!("{added}. Файл .torrent удалён.", "{added}. The .torrent file was deleted."),
                            Err(e) => tf!("{added}, но .torrent не удалён: {e}", "{added}, but the .torrent file was not deleted: {e}"),
                        },
                        None => added,
                    };
                }
                (AddKind::New { .. }, Err(error)) => self.notice = tf!("Не удалось добавить {}: {error}", "Could not add {}: {error}", outcome.label),
                (AddKind::Recheck { .. }, Ok((_, id, _))) => {
                    self.selected = id;
                    self.notice = t("Пересчёт хешей запущен: проверяются сохранённые части торрента", "Hash recheck started: verifying the pieces already on disk").into();
                }
                (AddKind::Recheck { torrent_bytes }, Err(error)) => {
                    // The torrent was already removed from the session; keep a
                    // copy of its .torrent so the user can add it back.
                    let recovery = app_data_dir().join("recovery").join(format!("{}.torrent", safe_folder_name(&outcome.label)));
                    let saved = recovery.parent().map(fs::create_dir_all).transpose().and_then(|_| fs::write(&recovery, &torrent_bytes));
                    self.notice = match saved {
                        Ok(()) => tf!("Не удалось перезапустить проверку хешей: {error}. Файлы на диске целы; .torrent сохранён в {}", "Could not restart the hash check: {error}. Files on disk are intact; the .torrent was saved to {}", recovery.display()),
                        Err(e) => tf!("Не удалось перезапустить проверку хешей: {error}; сохранить .torrent тоже не вышло: {e}", "Could not restart the hash check: {error}; saving the .torrent failed too: {e}"),
                    };
                }
            }
        }
    }

    fn remove_from_list(&mut self, id: usize, delete_files: bool) {
        self.notice = match self.block_on(self.session.delete(id.into(), delete_files)) {
            Ok(()) if delete_files => t("Торрент и его файлы удалены", "The torrent and its files were deleted").into(),
            Ok(()) => t("Торрент удалён из списка; файлы сохранены", "The torrent was removed from the list; files were kept").into(),
            Err(error) if delete_files => tf!("Удаление файлов завершилось с ошибкой: {error}", "Deleting files failed: {error}"),
            Err(error) => tf!("Не удалось удалить торрент: {error}", "Could not remove the torrent: {error}"),
        };
        if self.selected == Some(id) { self.selected = None; }
        self.last_stats_refresh = None;
    }

    fn collect_debug_snapshot(&self) -> DebugSnapshot {
        let session_debug = self.session.stats_snapshot();
        let dht_debug = self.session.get_dht().map(|dht| dht.stats());
        let tcp = &session_debug.connections.tcp.v4;
        let utp = &session_debug.connections.utp.v4;
        let network_lines = vec![
            match &dht_debug {
                Some(dht) => tf!("DHT: {} узлов в таблице · {} запросов в ожидании", "DHT: {} nodes in table · {} requests pending", dht.routing_table_size, dht.outstanding_requests),
                None => t("DHT: отключён", "DHT: disabled").into(),
            },
            tf!("TCP IPv4: попыток {} · успешно {} · ошибок {}", "TCP IPv4: attempts {} · succeeded {} · errors {}", tcp.attempts, tcp.successes, tcp.errors),
            tf!("uTP IPv4: попыток {} · успешно {} · ошибок {}", "uTP IPv4: attempts {} · succeeded {} · errors {}", utp.attempts, utp.successes, utp.errors),
            tf!("Сессия: скачано {} байт · заблокировано входящих {} · исходящих {}", "Session: downloaded {} bytes · blocked incoming {} · outgoing {}", session_debug.counters.fetched_bytes, session_debug.counters.blocked_incoming, session_debug.counters.blocked_outgoing),
        ];
        let torrents = self.session.with_torrents(|torrents| torrents.map(|(id, torrent)| {
            let stats = torrent.stats();
            let progress = if stats.total_bytes == 0 { 0.0 } else { stats.progress_bytes as f64 / stats.total_bytes as f64 * 100.0 };
            let is_private = torrent.with_metadata(|meta| meta.info.info().private).unwrap_or(false);
            let mut trackers = torrent.shared().trackers.iter().map(ToString::to_string).collect::<Vec<_>>();
            if !is_private {
                for tracker in fallback_trackers(&self.settings) {
                    if !trackers.contains(&tracker) { trackers.push(tracker); }
                }
            }
            trackers.sort();
            let trackers = trackers.iter().map(|tracker| safe_tracker_display(tracker)).collect();
            let hash = torrent.info_hash().0.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
            let state = if stats.finished { t("завершено", "finished") } else if torrent.is_paused() { t("пауза", "paused") } else { t("активно", "active") };
            let peers = stats.live.as_ref().map(|live| &live.snapshot.peer_stats)
                .map(|p| tf!("Пиры: найдено {} · в очереди {} · соединяются {} · подключено {} · недоступно {}", "Peers: seen {} · queued {} · connecting {} · connected {} · unreachable {}", p.seen, p.queued, p.connecting, p.live, p.dead))
                .unwrap_or_else(|| t("Пиры: торрент ещё инициализируется или на паузе", "Peers: the torrent is still initializing or paused").into());
            DebugTorrent {
                title: format!("{} · #{id}", torrent.name().unwrap_or_else(|| t("Метаданные…", "Metadata…").into())),
                lines: vec![tf!("Хэш: {hash}", "Hash: {hash}"), tf!("Состояние: {state} · {progress:.2}%", "State: {state} · {progress:.2}%"), peers],
                error: stats.error.clone(),
                trackers,
            }
        }).collect::<Vec<_>>());
        let mut report = String::from(t("Торрент-зверёк — диагностика сети\n", "Torrent Critter — network diagnostics\n"));
        for line in &network_lines { report.push_str(line); report.push('\n'); }
        for torrent in &torrents {
            report.push_str(&tf!("\nЗагрузка {}\n", "\nDownload {}\n", torrent.title));
            for line in &torrent.lines { report.push_str(line); report.push('\n'); }
            if let Some(error) = &torrent.error { report.push_str(&tf!("Ошибка: {error}\n", "Error: {error}\n")); }
            report.push_str(t("Трекеры:\n", "Trackers:\n"));
            for tracker in &torrent.trackers { report.push_str(&format!("  {tracker}\n")); }
        }
        DebugSnapshot { report, network_lines, torrents }
    }

    fn save_settings_window(&mut self, custom_trackers: Vec<String>) {
        let previous_trackers = self.settings_backup.as_ref().map(fallback_trackers);
        self.settings.custom_trackers = custom_trackers;
        let trackers_changed = previous_trackers.is_some_and(|previous| previous != fallback_trackers(&self.settings));
        let upnp_changed = self.settings.enable_upnp_port_forwarding != self.saved_upnp_port_forwarding;
        match save_settings(&self.settings, self.saved_autostart) {
            Ok(()) => {
                let mut pending_restart = Vec::new();
                if upnp_changed { pending_restart.push("UPnP"); }
                // Session-wide trackers are fixed when the session starts;
                // new downloads get the updated list right away.
                if trackers_changed { pending_restart.push(t("трекеры для уже добавленных загрузок", "trackers for downloads already in the list")); }
                self.notice = if pending_restart.is_empty() { t("Настройки сохранены", "Settings saved").into() } else {
                    tf!("Настройки сохранены. После перезапуска клиента применятся: {}.", "Settings saved. These take effect after restarting the client: {}.", pending_restart.join(", "))
                };
                self.saved_autostart = self.settings.autostart;
                self.saved_upnp_port_forwarding = self.settings.enable_upnp_port_forwarding;
            }
            Err(error) => self.notice = tf!("Не удалось сохранить настройки: {error}", "Could not save settings: {error}"),
        }
        self.settings_backup = None;
        self.show_settings = false;
    }

    /// Applies the chosen language (also while previewing it in the settings window).
    fn sync_language(&mut self, ctx: &egui::Context) {
        i18n::set_language(self.settings.language);
        if self.applied_language != self.settings.language {
            self.applied_language = self.settings.language;
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(app_name().into()));
            if let Some(tray) = &self.tray { tray.refresh_language(); }
        }
    }

    /// Minimize-to-tray and close-to-tray.
    fn handle_window_state(&mut self, ctx: &egui::Context) {
        if self.tray.is_none() { return; }
        let (minimized, close_requested) = ctx.input(|input| (input.viewport().minimized == Some(true), input.viewport().close_requested()));
        if minimized && self.settings.minimize_to_tray { desktop::hide_window(); }
        if close_requested && self.settings.close_to_tray && !desktop::QUIT_REQUESTED.load(std::sync::atomic::Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            desktop::hide_window();
        }
    }

    /// A .torrent path or magnet link passed on the command line of a later launch.
    fn open_argument(&mut self, argument: &str) {
        if argument.is_empty() { return; }
        if argument.to_ascii_lowercase().starts_with("magnet:") {
            self.queue_source(PendingSource::Magnet(argument.to_owned()));
        } else {
            self.add_file_path(PathBuf::from(argument));
        }
    }

    fn open_settings(&mut self) {
        if !self.show_settings {
            self.settings_backup = Some(self.settings.clone());
            self.trackers_text = self.settings.custom_trackers.join("
");
        }
        if !self.show_settings { self.center_settings = true; }
        self.show_settings = true;
    }

    fn add_file(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Torrent", &["torrent"]).pick_file() else { return; };
        self.add_file_path(path);
    }

    fn add_file_path(&mut self, path: PathBuf) {
        if !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("torrent")) {
            self.notice = t("Сюда можно перетащить только .torrent-файл", "Only .torrent files can be dropped here").into();
            return;
        }
        self.queue_source(PendingSource::File(path));
    }

    fn queue_source(&mut self, source: PendingSource) {
        if self.settings.use_default_download_dir {
            let base = self.settings.download_dir.clone();
            self.start_pending_source(source, base);
        } else {
            self.pending_folder = self.settings.download_dir.clone();
            self.pending_use_default = false;
            self.pending_source = Some(source);
            self.center_pending = true;
        }
    }

    fn start_pending_source(&mut self, pending: PendingSource, base_folder: String) {
        let (source, source_file, trackers, name, info_hash) = match pending {
            PendingSource::File(path) => match fs::read(&path) {
                Ok(bytes) => {
                    let meta = librqbit::torrent_from_bytes(&bytes).ok();
                    let info_hash = meta.as_ref().map(|meta| meta.info_hash.as_string());
                    let extra = fallback_trackers(&self.settings);
                    let trackers = match &meta { Some(meta) if !meta.info.data.private && !extra.is_empty() => Some(extra), _ => None };
                    let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or(t("Загрузка", "Download")).to_owned();
                    (AddTorrent::from_bytes(bytes), Some(path), trackers, name, info_hash)
                }
                Err(error) => { self.notice = tf!("Не удалось прочитать файл: {error}", "Could not read the file: {error}"); return; }
            },
            PendingSource::Magnet(uri) => {
                let params = magnet_params(&uri);
                let name = params.iter().find(|(k, v)| k == "dn" && !v.trim().is_empty()).map(|(_, v)| safe_folder_name(v)).unwrap_or_else(|| t("Magnet-загрузка", "Magnet download").into());
                let has_tracker = params.iter().any(|(k, _)| k.eq_ignore_ascii_case("tr"));
                let extra = fallback_trackers(&self.settings);
                let trackers = if !has_tracker && !extra.is_empty() { Some(extra) } else { None };
                let info_hash = librqbit::Magnet::parse(&uri).ok().and_then(|magnet| magnet.as_id20()).map(|id| id.as_string());
                (AddTorrent::from_url(uri), None, trackers, name, info_hash)
            }
        };
        let output_folder = PathBuf::from(base_folder).join(safe_folder_name(&name));
        if let Err(error) = fs::create_dir_all(&output_folder) { self.notice = tf!("Не удалось создать папку загрузки: {error}", "Could not create the download folder: {error}"); return; }
        // Lets the storage find this torrent's files (see storage.rs).
        if let Some(hash) = &info_hash { storage::register_output_folder(hash, output_folder.clone()); }
        let options = AddTorrentOptions {
            output_folder: Some(output_folder.to_string_lossy().into_owned()),
            overwrite: true,
            trackers,
            ..Default::default()
        };
        self.notice = tf!("Добавляем: {name}", "Adding: {name}");
        self.spawn_add(name, source, options, AddKind::New { source_file });
    }

    fn rows(&self) -> Vec<DownloadRow> {
        let dht_nodes = self.session.get_dht().map(|dht| dht.stats().routing_table_size).unwrap_or(0);
        self.session.with_torrents(|torrents| torrents.map(|(id, torrent)| {
            let stats = torrent.stats();
            let name = torrent.name().unwrap_or_else(|| t("Получение метаданных…", "Fetching metadata…").into());
            let progress = if stats.total_bytes == 0 { 0.0 } else { stats.progress_bytes as f32 / stats.total_bytes as f32 };
            let (live, seen, connecting, dead) = stats.live.as_ref().map(|live| (
                live.snapshot.peer_stats.live,
                live.snapshot.peer_stats.seen,
                live.snapshot.peer_stats.connecting,
                live.snapshot.peer_stats.dead,
            )).unwrap_or((0, 0, 0, 0));
            let detail = if let Some(error) = &stats.error {
                tf!("Ошибка: {error}", "Error: {error}")
            } else if torrent.is_paused() {
                t("На паузе", "Paused").into()
            } else if stats.finished {
                t("Готово · раздаётся", "Done · seeding").into()
            } else if live > 0 {
                tf!("Пиров подключено: {live} · найдено: {seen}", "Peers connected: {live} · seen: {seen}")
            } else if connecting > 0 {
                tf!("Пиров найдено: {seen} · подключаемся: {connecting} · недоступно: {dead}", "Peers seen: {seen} · connecting: {connecting} · unreachable: {dead}")
            } else if seen > 0 {
                tf!("Пиров найдено: {seen} · не ответили: {dead} · узлов DHT: {dht_nodes}", "Peers seen: {seen} · not responding: {dead} · DHT nodes: {dht_nodes}")
            } else if stats.total_bytes == 0 {
                tf!("Получаем метаданные · узлов DHT: {dht_nodes}", "Fetching metadata · DHT nodes: {dht_nodes}")
            } else {
                tf!("Ищем пиры · узлов DHT: {dht_nodes}", "Looking for peers · DHT nodes: {dht_nodes}")
            };
            let raw_speed_mib_s = stats.live.as_ref().map(|live| live.download_speed.mbps).unwrap_or(0.0);
            DownloadRow {
                id,
                name,
                progress: progress.clamp(0.0, 1.0),
                detail,
                active: !torrent.is_paused() && !stats.finished,
                finished: stats.finished,
                paused: torrent.is_paused(),
                peers: seen,
                downloaded: format_bytes(stats.progress_bytes),
                total: format_bytes(stats.total_bytes),
                speed: String::new(),
                eta: String::new(),
                raw_speed_mib_s,
                remaining_bytes: stats.total_bytes.saturating_sub(stats.progress_bytes),
                live_peers: live,
            }
        }).collect())
    }

    fn smooth_speeds(&mut self, rows: &mut [DownloadRow], raw_download: f64, raw_upload: f64) {
        let now = Instant::now();
        let elapsed = self.last_speed_sample.map(|last| now.duration_since(last)).unwrap_or(Duration::ZERO);
        if self.last_speed_sample.is_none() {
            self.smoothed_download_mib_s = raw_download;
            self.smoothed_upload_mib_s = raw_upload;
            for row in rows.iter() { self.smoothed_torrent_speeds.insert(row.id, row.raw_speed_mib_s); }
            self.last_speed_sample = Some(now);
        } else if elapsed >= Duration::from_millis(500) {
            // EMA with a 2.5 s time constant; sample at most twice per second.
            let alpha = 1.0 - (-(elapsed.as_secs_f64()) / 2.5).exp();
            self.smoothed_download_mib_s += alpha * (raw_download - self.smoothed_download_mib_s);
            self.smoothed_upload_mib_s += alpha * (raw_upload - self.smoothed_upload_mib_s);
            let active_ids: std::collections::HashSet<_> = rows.iter().map(|row| row.id).collect();
            self.smoothed_torrent_speeds.retain(|id, _| active_ids.contains(id));
            for row in rows.iter() {
                let current = self.smoothed_torrent_speeds.entry(row.id).or_insert(row.raw_speed_mib_s);
                *current += alpha * (row.raw_speed_mib_s - *current);
            }
            self.last_speed_sample = Some(now);
        }
        for row in rows {
            let speed = self.smoothed_torrent_speeds.get(&row.id).copied().unwrap_or(0.0);
            row.speed = format_speed(speed);
            row.eta = format_eta(row.remaining_bytes, speed);
        }
    }

    fn advance_cat_herds(&mut self, rows: &[DownloadRow], lane_width: f32) {
        let now = Instant::now();
        let delta = self.last_cat_frame.map(|last| now.duration_since(last).as_secs_f32().clamp(0.0, 0.1)).unwrap_or(0.0);
        let lane_width = lane_width.max(1.0);
        self.last_cat_frame = Some(now);
        let active_ids: std::collections::HashSet<_> = rows.iter()
            .filter(|row| row.active && self.settings.cats_pack_race)
            .map(|row| row.id)
            .collect();
        self.cat_herds.retain(|id, _| active_ids.contains(id));
        for row in rows.iter().filter(|row| row.active && self.settings.cats_pack_race) {
            let desired = row.live_peers.min(128) as usize;
            let herd = self.cat_herds.entry(row.id).or_insert_with(|| {
                // The first pack also enters from off-screen on the left.
                // Later peers join behind it without moving existing runners.
                (0..desired).map(|index| CatRunner {
                    position: -0.08 - index as f32 * 0.025,
                    speed_px_s: 130.0 + ((index * 37) % 13) as f32 * 8.0,
                    retiring: false,
                    coat_index: index % 6,
                }).collect()
            });
            let running = herd.iter().filter(|cat| !cat.retiring).count();
            if desired < running {
                let mut candidates: Vec<_> = herd.iter().enumerate().filter(|(_, cat)| !cat.retiring).map(|(index, cat)| (index, cat.position)).collect();
                candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
                for (index, _) in candidates.into_iter().take(running - desired) { herd[index].retiring = true; }
            } else if desired > running {
                let reactivate = (desired - running).min(herd.iter().filter(|cat| cat.retiring).count());
                for cat in herd.iter_mut().filter(|cat| cat.retiring).take(reactivate) { cat.retiring = false; }
                let added = desired.saturating_sub(running + reactivate);
                for _ in 0..added {
                    let index = herd.len();
                    let waiting = herd.iter().filter(|cat| cat.position < 0.0).count();
                    herd.push(CatRunner {
                        position: -0.08 - waiting as f32 * 0.035,
                        speed_px_s: 130.0 + ((index * 37) % 13) as f32 * 8.0,
                        retiring: false,
                        coat_index: index % 6,
                    });
                }
            }
            for cat in herd.iter_mut() {
                cat.position += cat.speed_px_s * delta / lane_width;
                if cat.position > 1.015 {
                    if !cat.retiring {
                        // The runner completes the whole bar, then re-enters from
                        // the left like a new cat in the race.
                        cat.position = -0.08;
                    }
                }
            }
            herd.retain(|cat| !(cat.retiring && cat.position > 1.015));
        }
    }

    fn recheck_torrent(&mut self, id: usize) {
        let Some(torrent) = self.session.get(id.into()) else { return; };
        let torrent_bytes = match torrent.with_metadata(|metadata| metadata.torrent_bytes.clone()) {
            Ok(bytes) => bytes.to_vec(),
            Err(error) => { self.notice = tf!("Нельзя пересчитать хеши до получения метаданных: {error}", "Hashes cannot be rechecked before metadata arrives: {error}"); return; }
        };
        storage::register_output_folder(&torrent.info_hash().as_string(), torrent.output_folder().to_path_buf());
        let output_folder = torrent.output_folder().to_string_lossy().into_owned();
        let trackers = torrent.shared().trackers.iter().map(ToString::to_string).collect();
        let name = torrent.name().unwrap_or_else(|| t("Загрузка", "Download").into());
        let paused = torrent.is_paused();
        if !paused {
            if let Err(error) = self.block_on(self.session.pause(&torrent)) {
                self.notice = tf!("Не удалось приостановить загрузку для проверки: {error}", "Could not pause the download for the check: {error}");
                return;
            }
        }
        if let Err(error) = self.block_on(self.session.delete(id.into(), false)) {
            self.notice = tf!("Не удалось подготовить загрузку к проверке: {error}", "Could not prepare the download for the check: {error}");
            return;
        }
        let options = AddTorrentOptions {
            output_folder: Some(output_folder),
            trackers: Some(trackers),
            overwrite: true,
            paused,
            ..Default::default()
        };
        self.selected = None;
        self.notice = tf!("Готовим пересчёт хешей: {name}", "Preparing hash recheck: {name}");
        let source = AddTorrent::from_bytes(torrent_bytes.clone());
        self.spawn_add(name, source, options, AddKind::Recheck { torrent_bytes });
    }

    fn torrent_context_menu(&mut self, ui: &mut egui::Ui, id: usize) {
        let Some(torrent) = self.session.get(id.into()) else { return; };
        let paused = torrent.is_paused();
        let finished = torrent.stats().finished;
        if ui.button(t("Открыть папку загрузки", "Open download folder")).clicked() {
            let folder = torrent.output_folder().to_path_buf();
            match std::process::Command::new("explorer.exe").arg(folder).spawn() {
                Ok(_) => self.notice = t("Открыта папка загрузки", "Download folder opened").into(),
                Err(error) => self.notice = tf!("Не удалось открыть папку: {error}", "Could not open the folder: {error}"),
            }
            ui.close_menu();
        }
        let action_label = if paused {
            if finished { t("Запустить раздачу", "Start seeding") } else { t("Продолжить", "Resume") }
        } else if finished { t("Остановить раздачу", "Stop seeding") } else { t("Стоп", "Stop") };
        if ui.button(action_label).clicked() {
            self.toggle_torrent(id);
            ui.close_menu();
        }
        if ui.button(t("Пересчитать хеши частей", "Recheck piece hashes")).clicked() {
            self.recheck_torrent(id);
            ui.close_menu();
        }
        ui.separator();
        if ui.button(t("Удалить только из списка", "Remove from list only")).clicked() {
            self.remove_from_list(id, false);
            ui.close_menu();
        }
        if ui.button(t("Удалить торрент и скачанные файлы…", "Remove torrent and downloaded files…")).clicked() {
            self.confirm_delete = Some((id, torrent.name().unwrap_or_else(|| t("этот торрент", "this torrent").into())));
            ui.close_menu();
        }
    }

    fn set_all_paused(&mut self, paused: bool) {
        let ids = self.session.with_torrents(|torrents| torrents.map(|(id, _)| id).collect::<Vec<_>>());
        let mut changed = 0usize;
        let mut failures = Vec::new();
        for id in ids {
            if let Some(torrent) = self.session.get(id.into()) {
                if torrent.is_paused() == paused { continue; }
                let result = if paused {
                    self.block_on(self.session.pause(&torrent))
                } else {
                    self.block_on(self.session.unpause(&torrent))
                };
                match result {
                    Ok(()) => changed += 1,
                    Err(error) => failures.push(error.to_string()),
                }
            }
        }
        self.notice = if failures.is_empty() {
            tf!("{} торрентов: {changed}", "{} torrents: {changed}", if paused { t("Остановлено", "Stopped") } else { t("Запущено", "Started") })
        } else {
            tf!("Изменено: {changed}; ошибок: {} — {}", "Changed: {changed}; errors: {} — {}", failures.len(), failures[0])
        };
        self.last_stats_refresh = None;
    }

    fn toggle_torrent(&mut self, id: usize) {
        let Some(torrent) = self.session.get(id.into()) else {
            self.notice = t("Торрент уже удалён", "The torrent was already removed").into();
            return;
        };
        let was_paused = torrent.is_paused();
        let finished = torrent.stats().finished;
        let result = if was_paused {
            self.block_on(self.session.unpause(&torrent))
        } else {
            self.block_on(self.session.pause(&torrent))
        };
        self.notice = match result {
            Ok(()) if was_paused && finished => t("Раздача запущена", "Seeding started").into(),
            Ok(()) if was_paused => t("Загрузка продолжена", "Download resumed").into(),
            Ok(()) if finished => t("Раздача остановлена", "Seeding stopped").into(),
            Ok(()) => t("Загрузка остановлена", "Download stopped").into(),
            Err(error) => tf!("Не удалось изменить состояние торрента: {error}", "Could not change the torrent state: {error}"),
        };
        self.last_stats_refresh = None;
    }
}

impl eframe::App for TorrentClientApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.sync_language(ctx);
        self.handle_window_state(ctx);
        if self.hide_at_startup {
            // eframe makes the window visible after painting the first frame,
            // so hide it on the frame after that.
            if ctx.cumulative_pass_nr() >= 1 { self.hide_at_startup = false; desktop::hide_window(); } else { ctx.request_repaint(); }
        }
        for argument in desktop::take_inbox() { self.open_argument(argument.trim()); }
        self.process_add_outcomes();
        if let Some(path) = self.pending_initial_file.take() { self.open_argument(&path.to_string_lossy()); }
        let dropped = ctx.input(|input| input.raw.dropped_files.clone());
        for file in dropped {
            if let Some(path) = file.path { self.add_file_path(path); }
        }
        let is_dragging_file = ctx.input(|input| !input.raw.hovered_files.is_empty());
        let refresh_stats = self.last_stats_refresh.map_or(true, |last| last.elapsed() >= Duration::from_millis(500));
        let mut rows = if refresh_stats {
            let mut fresh_rows = self.rows();
            let session_stats = self.session.stats_snapshot();
            self.smooth_speeds(&mut fresh_rows, session_stats.download_speed.mbps, session_stats.upload_speed.mbps);
            self.cached_rows = fresh_rows;
            self.last_stats_refresh = Some(Instant::now());
            self.cached_rows.clone()
        } else {
            self.cached_rows.clone()
        };
        match self.download_sort {
            DownloadSort::Added => rows.sort_by_key(|row| row.id),
            DownloadSort::Name => rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            DownloadSort::Progress => rows.sort_by(|a, b| b.progress.total_cmp(&a.progress)),
            DownloadSort::Speed => rows.sort_by(|a, b| b.raw_speed_mib_s.total_cmp(&a.raw_speed_mib_s)),
            DownloadSort::Peers => rows.sort_by(|a, b| b.peers.cmp(&a.peers)),
        }
        self.advance_cat_herds(&rows, ctx.screen_rect().width());
        let animating = !self.in_flight.is_empty() || rows.iter().any(|row| row.active || (row.finished && !row.paused));
        let animation_time = ctx.input(|input| input.time);

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            egui::Frame::new().fill(Color32::from_rgb(255, 225, 178)).inner_margin(6.0).corner_radius(CornerRadius::same(13)).show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(5.0, 3.0);
                // A Frame otherwise shrink-wraps this wrapped content to the
                // controls' natural width. Reserve the full viewport width so
                // the colored header remains responsive as the window resizes.
                let toolbar_width = ui.available_width();
                ui.set_min_width(toolbar_width);
                let wide_header = toolbar_width >= 760.0;
                ui.horizontal_wrapped(|ui| {
                    draw_mascot(ui);
                    ui.vertical(|ui| {
                        ui.heading(egui::RichText::new(t("Торрент-зверёк", "Torrent Critter")).size(19.0).strong().color(Color32::from_rgb(115, 67, 42)));
                        ui.label(egui::RichText::new(t("Загружаем добро вместе!", "Fetching good stuff together!")).size(11.0));
                    });
                    if wide_header {
                        if cat_menu_button(ui, t("Добавить .torrent", "Add .torrent"), CatIconKind::Add).clicked() { self.add_file(); }
                        if cat_menu_button(ui, t("Настройки", "Settings"), CatIconKind::Settings).clicked() { self.open_settings(); }
                        if cat_menu_button(ui, t("Выход", "Exit"), CatIconKind::Exit).clicked() { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
                    }
                });
                if !wide_header {
                    ui.horizontal_wrapped(|ui| {
                        if cat_menu_button(ui, t("Добавить .torrent", "Add .torrent"), CatIconKind::Add).clicked() { self.add_file(); }
                        if cat_menu_button(ui, t("Настройки", "Settings"), CatIconKind::Settings).clicked() { self.open_settings(); }
                        if cat_menu_button(ui, t("Выход", "Exit"), CatIconKind::Exit).clicked() { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    draw_stat_chip(ui, t("Скачивание", "Download"), format_speed(self.smoothed_download_mib_s), Color32::from_rgb(226, 246, 229), Color32::from_rgb(49, 103, 63), CatIconKind::Download);
                    draw_stat_chip(ui, t("Отдача", "Upload"), format_speed(self.smoothed_upload_mib_s), Color32::from_rgb(230, 239, 250), Color32::from_rgb(57, 82, 120), CatIconKind::Upload);
                    draw_stat_chip(ui, t("Загрузки", "Downloads"), tf!("{} активно · {} всего", "{} active · {} total", rows.iter().filter(|row| row.active).count(), rows.len()), Color32::from_rgb(255, 246, 224), Color32::from_rgb(112, 81, 42), CatIconKind::Add);
                });
                let available_magnet_width = ui.available_width().min(480.0);
                let magnet_field_width = (available_magnet_width - 190.0).clamp(90.0, 300.0);
                let magnet_width = (magnet_field_width + 153.0).min(available_magnet_width);
                egui::Frame::new()
                    .fill(Color32::from_rgb(255, 244, 224))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(239, 207, 159)))
                    .corner_radius(CornerRadius::same(11))
                    .inner_margin(egui::vec2(4.0, 2.0))
                    .show(ui, |ui| {
                        ui.set_min_width((magnet_width - 8.0).max(0.0));
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
                            draw_cat_icon(ui, CatIconKind::Add, 16.0);
                            ui.label(egui::RichText::new("MAGNET").size(9.0).strong().color(Color32::from_rgb(130, 87, 53)));
                            ui.add_sized([magnet_field_width, 24.0], egui::TextEdit::singleline(&mut self.magnet_text)
                                .hint_text(t("Magnet-ссылка", "Magnet link"))
                                .vertical_align(egui::Align::Center));
                            if ui.add_sized([78.0, 26.0], egui::Button::new(egui::RichText::new(t("Добавить", "Add")).size(12.0)).corner_radius(CornerRadius::same(7))).clicked() {
                                let uri = self.magnet_text.trim().to_owned();
                                if uri.starts_with("magnet:?") {
                                    self.queue_source(PendingSource::Magnet(uri));
                                    self.magnet_text.clear();
                                } else if !uri.is_empty() {
                                    self.notice = t("Ссылка должна начинаться с magnet:?", "The link must start with magnet:?").into();
                                }
                            }
                        });
                    });
            });
        });

        if is_dragging_file {
            egui::Window::new(t("Перетащите торрент сюда", "Drop a torrent here"))
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .collapsible(false).resizable(false).show(ctx, |ui| {
                    ui.label(t("Отпустите .torrent-файл, чтобы добавить его в загрузки", "Release the .torrent file to add it to downloads"));
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                egui::Frame::new().fill(Color32::from_rgb(255, 253, 246)).stroke(Stroke::new(1.0_f32, Color32::from_rgb(244, 222, 191))).corner_radius(CornerRadius::same(18)).inner_margin(16.0).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.heading(t("Мои загрузки", "My downloads"));
                        ui.add_space(8.0);
                        if ui.button(t("▶ Запустить все", "▶ Start all")).clicked() { self.set_all_paused(false); }
                        if ui.button(t("■ Остановить все", "■ Stop all")).clicked() { self.set_all_paused(true); }
                        egui::ComboBox::from_id_salt("download_list_sort")
                            .selected_text(self.download_sort.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.download_sort, DownloadSort::Added, DownloadSort::Added.label());
                                ui.selectable_value(&mut self.download_sort, DownloadSort::Name, DownloadSort::Name.label());
                                ui.selectable_value(&mut self.download_sort, DownloadSort::Progress, DownloadSort::Progress.label());
                                ui.selectable_value(&mut self.download_sort, DownloadSort::Speed, DownloadSort::Speed.label());
                                ui.selectable_value(&mut self.download_sort, DownloadSort::Peers, DownloadSort::Peers.label());
                            });
                    });
                    ui.add_space(8.0);
                    let mut cancel_token = None;
                    for add in &self.in_flight {
                        egui::Frame::new()
                            .fill(Color32::from_rgb(255, 247, 231))
                            .corner_radius(CornerRadius::same(12))
                            .inner_margin(10.0)
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.horizontal_wrapped(|ui| {
                                    ui.spinner();
                                    ui.label(egui::RichText::new(&add.label).size(16.0).strong());
                                    ui.label(egui::RichText::new(tf!("Получаем метаданные от пиров… {} с", "Fetching metadata from peers… {} s", add.started.elapsed().as_secs())).size(14.0));
                                    if ui.button(t("Отменить", "Cancel")).clicked() { cancel_token = Some(add.token); }
                                });
                            });
                        ui.add_space(6.0);
                    }
                    if let Some(token) = cancel_token { self.cancel_in_flight(token); }
                    for row in &rows {
                        let id = row.id;
                        let selected = self.selected == Some(id);
                        let cat_runners = self.cat_herds.get(&id).cloned().unwrap_or_default();
                        let mut row_action_clicked = false;
                        let mut action_rects = Vec::new();
                        let mut toggle_rect = None;
                        let card = egui::Frame::new()
                            .fill(if selected { Color32::from_rgb(235, 249, 237) } else { Color32::from_rgb(255, 247, 231) })
                            .corner_radius(CornerRadius::same(12))
                            .inner_margin(10.0)
                            .show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    let name_width = (ui.available_width() * 0.62).min(320.0).max(120.0);
                                    let name_response = ui.add_sized([name_width, 32.0], egui::Button::new(egui::RichText::new(&row.name).size(16.0).strong()).selected(selected));
                                    name_response.on_hover_text(t("Левый клик — показать/скрыть действия; правый — меню", "Left click shows/hides actions; right click opens the menu"));
                                    ui.label(egui::RichText::new(&row.detail).size(14.0));
                                });
                                ui.horizontal_wrapped(|ui| {
                                    draw_cat_icon(ui, CatIconKind::Files, 20.0);
                                    ui.label(egui::RichText::new(format!("{} / {}", row.downloaded, row.total)).size(13.0).color(Color32::from_rgb(91, 82, 70)));
                                    ui.separator();
                                    draw_cat_icon(ui, CatIconKind::Download, 20.0);
                                    ui.label(egui::RichText::new(&row.speed).size(13.0).strong().color(Color32::from_rgb(49, 103, 63)));
                                    if row.active {
                                        ui.separator();
                                        ui.label(egui::RichText::new(tf!("Осталось: {}", "Remaining: {}", row.eta)).size(13.0).color(Color32::from_rgb(91, 82, 70)));
                                    }
                                });
                                ui.add_space(4.0);
                                draw_cat_progress(ui, row.progress, animation_time, row.active, row.finished && !row.paused, row.peers, row.live_peers, self.settings.cats_by_peers, self.settings.cats_pack_race, self.settings.yarn_when_seeding, row.id.wrapping_mul(3).wrapping_add(11), &cat_runners);
                                if selected {
                                    if self.session.get(id.into()).is_some() {
                                        ui.add_space(5.0);
                                        ui.horizontal_wrapped(|ui| {
                                            let toggle_label = if row.paused {
                                                if row.finished { t("Запустить раздачу", "Start seeding") } else { t("Продолжить", "Resume") }
                                            } else if row.finished { t("Остановить раздачу", "Stop seeding") } else { t("Стоп", "Stop") };
                                            let toggle = ui.button(toggle_label);
                                            toggle_rect = Some(toggle.rect);
                                            action_rects.push(toggle.rect);
                                            let recheck = ui.button(t("Пересчитать хеши", "Recheck hashes"));
                                            action_rects.push(recheck.rect);
                                            if recheck.clicked() {
                                                row_action_clicked = true;
                                                self.recheck_torrent(id);
                                            }
                                            let remove = ui.button(t("Удалить из списка", "Remove from list"));
                                            action_rects.push(remove.rect);
                                            if remove.clicked() {
                                                row_action_clicked = true;
                                                self.remove_from_list(id, false);
                                            }
                                            let hide = ui.button(t("Скрыть действия", "Hide actions"));
                                            action_rects.push(hide.rect);
                                            if hide.clicked() {
                                                row_action_clicked = true;
                                                self.selected = None;
                                            }
                                        });
                                    }
                                }
                            });
                        let pointer_click = ctx.input(|input| {
                            if input.pointer.button_clicked(egui::PointerButton::Primary) { input.pointer.interact_pos() } else { None }
                        });
                        let clicked_toggle = pointer_click.is_some_and(|pos| toggle_rect.is_some_and(|rect| rect.contains(pos)));
                        if clicked_toggle {
                            row_action_clicked = true;
                            self.toggle_torrent(id);
                        }
                        let clicked_action = pointer_click.is_some_and(|pos| action_rects.iter().any(|rect| rect.contains(pos)));
                        let left_clicked = pointer_click.is_some_and(|pos| card.response.rect.contains(pos));
                        if left_clicked && !row_action_clicked && !clicked_action { self.selected = if selected { None } else { Some(id) }; }
                        card.response.on_hover_text(t("Правый клик — действия с загрузкой", "Right click for download actions"))
                            .context_menu(|ui| self.torrent_context_menu(ui, id));
                        ui.add_space(6.0);
                    }
                    if rows.is_empty() && self.in_flight.is_empty() {
                        ui.label(t("Пока тихо — котики устроили перерыв. Добавьте .torrent или magnet-ссылку!", "All quiet — the cats are on a break. Add a .torrent or a magnet link!"));
                        ui.add_space(8.0);
                        draw_cat_progress(ui, 0.0, animation_time, false, false, 0, 0, self.settings.cats_by_peers, self.settings.cats_pack_race, false, std::process::id() as usize, &[]);
                    }
                });
            });
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            egui::Frame::new().fill(Color32::from_rgb(217, 242, 224)).inner_margin(8.0).corner_radius(CornerRadius::same(12)).show(ui, |ui| {
                ui.horizontal(|ui| { draw_cat_icon(ui, CatIconKind::Settings, 20.0); ui.label(&self.notice); });
            });
        });
        if self.show_settings {
            let mut window = egui::Window::new(t("Настройки", "Settings")).id(egui::Id::new("settings_window"))
                .collapsible(false).resizable(false).pivot(egui::Align2::CENTER_CENTER);
            // Open in the middle of the client window; the user may move it afterwards.
            if std::mem::take(&mut self.center_settings) { window = window.current_pos(ctx.screen_rect().center()); }
            window.show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(t("Язык / Language:", "Language / Язык:"));
                    egui::ComboBox::from_id_salt("language_picker")
                        .selected_text(self.settings.language.native_name())
                        .show_ui(ui, |ui| {
                            for language in Language::ALL { ui.selectable_value(&mut self.settings.language, language, language.native_name()); }
                        });
                });
                ui.separator();
                ui.checkbox(&mut self.settings.delete_torrent_after_add, t("Удалять .torrent после добавления", "Delete the .torrent file after adding"));
                ui.checkbox(&mut self.settings.autostart, t("Запускать вместе с Windows", "Start with Windows"));
                ui.add_enabled_ui(self.tray.is_some(), |ui| {
                    ui.checkbox(&mut self.settings.start_in_tray, t("Запускать свёрнутым в трей", "Start minimized to the tray"));
                    ui.checkbox(&mut self.settings.minimize_to_tray, t("Сворачивать в трей, а не на панель задач", "Minimize to the tray instead of the taskbar"));
                    ui.checkbox(&mut self.settings.close_to_tray, t("Крестик сворачивает в трей (выход — через меню значка)", "Closing the window hides it to the tray (exit from the tray menu)"));
                });
                ui.checkbox(&mut self.settings.add_public_trackers, t("Добавлять публичные трекеры для поиска пиров", "Add public trackers to find more peers"));
                ui.label(t("Свои трекеры (по одному адресу на строку):", "Your own trackers (one address per line):"));
                ui.add(egui::TextEdit::multiline(&mut self.trackers_text)
                    .hint_text("udp://tracker.example.org:1337/announce")
                    .desired_rows(3)
                    .desired_width(f32::INFINITY));
                ui.checkbox(&mut self.settings.enable_upnp_port_forwarding, t("Автоматически открыть входящий порт на роутере (UPnP)", "Open the incoming port on the router automatically (UPnP)"));
                ui.checkbox(&mut self.settings.use_default_download_dir, t("Использовать папку загрузок по умолчанию и не спрашивать при добавлении", "Use the default download folder and don't ask when adding"));
                ui.checkbox(&mut self.settings.show_debug_window, t("Показывать окно отладки сети", "Show the network debug window"));
                ui.label(t("Котики на полосе:", "Cats on the progress bar:"));
                let mut cat_mode = if self.settings.cats_pack_race { 2 } else if self.settings.cats_by_peers { 1 } else { 0 };
                ui.radio_value(&mut cat_mode, 0, t("Бегущие котики", "Running cats"));
                ui.radio_value(&mut cat_mode, 1, t("Клубок дерущихся котиков", "A tussling ball of cats"));
                ui.radio_value(&mut cat_mode, 2, t("Кошачья стая: по котику на подключённого пира", "Cat pack: one cat per connected peer"));
                self.settings.cats_by_peers = cat_mode == 1;
                self.settings.cats_pack_race = cat_mode == 2;
                ui.checkbox(&mut self.settings.yarn_when_seeding, t("Котик с клубком на раздаче", "A cat playing with yarn while seeding"));
                if ui.button(t("Сделать клиентом по умолчанию для .torrent", "Make default app for .torrent files")).clicked() {
                    self.notice = match register_torrent_association() {
                        Ok(()) => t("Клиент зарегистрирован. Выберите «Торрент-зверёк» в параметрах Windows, если система спросит.", "The client is registered. Pick “Torrent Critter” in Windows settings if asked.").into(),
                        Err(error) => tf!("Не удалось зарегистрировать приложение: {error}", "Could not register the app: {error}"),
                    };
                }
                ui.label(egui::RichText::new(t("Windows может попросить подтвердить приложение в настройках стандартных приложений.", "Windows may ask you to confirm the app in Default apps settings.")).small());
                ui.label(egui::RichText::new(t("Автозапуск настраивается только для этой учётной записи.", "Autostart is set up for this user account only.")).small());
                ui.label(egui::RichText::new(t("Private .torrent не получают ни публичные, ни свои трекеры.", "Private torrents get neither public nor your own trackers.")).small());
                ui.label(egui::RichText::new(t("UPnP может помочь принимать входящие соединения; настройка применяется после перезапуска.", "UPnP can help accept incoming connections; the change applies after a restart.")).small());
                ui.horizontal(|ui| {
                    ui.label(t("Папка загрузок:", "Download folder:")); ui.text_edit_singleline(&mut self.settings.download_dir);
                    if ui.button(t("Обзор…", "Browse…")).clicked() {
                        if let Some(folder) = rfd::FileDialog::new().pick_folder() { self.settings.download_dir = folder.to_string_lossy().into_owned(); }
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button(t("Сохранить", "Save")).clicked() {
                        match parse_tracker_list(&self.trackers_text) {
                            // Keep the window open so the user can fix the bad line.
                            Err(error) => self.notice = tf!("Настройки не сохранены: {error}", "Settings not saved: {error}"),
                            Ok(custom_trackers) => self.save_settings_window(custom_trackers),
                        }
                    }
                    if ui.button(t("Закрыть", "Close")).clicked() {
                        // Discard unsaved edits so they cannot be persisted later
                        // by an unrelated save (e.g. "use this folder by default").
                        if let Some(backup) = self.settings_backup.take() { self.settings = backup; }
                        self.show_settings = false;
                    }
                });
            });
        }
        if self.pending_source.is_some() {
            let mut window = egui::Window::new(t("Куда сохранить загрузку?", "Where to save the download?")).id(egui::Id::new("pending_folder_window"))
                .collapsible(false).resizable(false).pivot(egui::Align2::CENTER_CENTER);
            if std::mem::take(&mut self.center_pending) { window = window.current_pos(ctx.screen_rect().center()); }
            window.show(ctx, |ui| {
                ui.label(t("Для торрента будет создана отдельная подпапка по имени .torrent-файла.", "A separate subfolder named after the .torrent file will be created."));
                ui.horizontal(|ui| {
                    ui.add_sized([280.0, 28.0], egui::TextEdit::singleline(&mut self.pending_folder));
                    if ui.button(t("Обзор…", "Browse…")).clicked() {
                        if let Some(folder) = rfd::FileDialog::new().set_directory(&self.pending_folder).pick_folder() { self.pending_folder = folder.to_string_lossy().into_owned(); }
                    }
                });
                ui.checkbox(&mut self.pending_use_default, t("Использовать эту папку по умолчанию для следующих загрузок", "Use this folder by default for future downloads"));
                ui.horizontal(|ui| {
                    if ui.button(t("Начать загрузку", "Start download")).clicked() {
                        if self.pending_use_default {
                            // Persist only this change on top of the last saved
                            // settings, not edits still pending in the settings window.
                            let mut to_save = self.settings_backup.clone().unwrap_or_else(|| self.settings.clone());
                            to_save.download_dir = self.pending_folder.clone();
                            to_save.use_default_download_dir = true;
                            for settings in std::iter::once(&mut self.settings).chain(self.settings_backup.as_mut()) {
                                settings.download_dir = self.pending_folder.clone();
                                settings.use_default_download_dir = true;
                            }
                            if let Err(error) = save_settings(&to_save, self.saved_autostart) { self.notice = tf!("Не удалось сохранить настройки: {error}", "Could not save settings: {error}"); }
                        }
                        if let Some(source) = self.pending_source.take() { self.start_pending_source(source, self.pending_folder.clone()); }
                    }
                    if ui.button(t("Отмена", "Cancel")).clicked() { self.pending_source = None; }
                });
            });
        }
        if let Some((id, name)) = self.confirm_delete.clone() {
            let mut decision = None;
            egui::Window::new(t("Удалить файлы?", "Delete files?")).collapsible(false).resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0)).show(ctx, |ui| {
                    ui.label(tf!("«{name}» будет удалён из списка вместе со скачанными файлами.", "“{name}” will be removed from the list together with its downloaded files."));
                    ui.label(egui::RichText::new(t("Файлы удаляются с диска насовсем, не в корзину.", "Files are deleted from disk permanently, not moved to the Recycle Bin.")).strong().color(Color32::DARK_RED));
                    ui.horizontal(|ui| {
                        if ui.button(t("Удалить файлы", "Delete files")).clicked() { decision = Some(true); }
                        if ui.button(t("Отмена", "Cancel")).clicked() { decision = Some(false); }
                    });
                });
            if let Some(confirmed) = decision {
                self.confirm_delete = None;
                if confirmed { self.remove_from_list(id, true); }
            }
        }
        if self.settings.show_debug_window {
            // Diagnostics scan torrent metadata and tracker lists; rebuild the
            // snapshot only together with the download list (every 500 ms),
            // not on every 30 fps repaint.
            if refresh_stats || self.debug_cache.is_none() { self.debug_cache = Some(self.collect_debug_snapshot()); }
            let snapshot = self.debug_cache.as_ref().expect("debug snapshot was just filled");
            let mut copy_clicked = false;
            egui::Window::new(t("Отладка сети", "Network debug"))
                .default_width(540.0)
                .default_height(420.0)
                .open(&mut self.settings.show_debug_window)
                .show(ctx, |ui| {
                    ui.label(t("Диагностика соединений, обновляется вместе со списком загрузок.", "Connection diagnostics, refreshed together with the download list."));
                    ui.horizontal(|ui| {
                        if ui.button(t("Скопировать отчёт", "Copy report")).clicked() {
                            ctx.copy_text(snapshot.report.clone());
                            copy_clicked = true;
                        }
                        if !self.debug_copy_notice.is_empty() { ui.small(&self.debug_copy_notice); }
                    });
                    ui.separator();
                    ui.heading(t("Сеть и соединения", "Network and connections"));
                    for line in &snapshot.network_lines { ui.label(line); }
                    ui.separator();
                    ui.heading(t("Загрузки", "Downloads"));
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for torrent in &snapshot.torrents {
                            egui::CollapsingHeader::new(&torrent.title).default_open(true).show(ui, |ui| {
                                for line in &torrent.lines { ui.label(line); }
                                if let Some(error) = &torrent.error { ui.colored_label(Color32::DARK_RED, tf!("Ошибка: {error}", "Error: {error}")); }
                                ui.label(t("Трекеры:", "Trackers:"));
                                for tracker in &torrent.trackers { ui.small(tracker); }
                            });
                        }
                    });
                });
            if copy_clicked { self.debug_copy_notice = t("Отчёт скопирован в буфер обмена", "Report copied to the clipboard").into(); }
        } else {
            self.debug_cache = None;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(if animating { 33 } else { 100 }));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(runtime) = self.runtime.take() {
            // Torrent/session state is persisted while the client is running;
            // cancel tasks directly here so window closing never waits on peers or disk I/O.
            self.session.cancellation_token().cancel();
            runtime.shutdown_timeout(std::time::Duration::from_millis(40));
        }
    }
}

fn main() -> eframe::Result<()> {
    let settings = load_settings();
    i18n::set_language(settings.language);
    // A second launch (e.g. double-clicking a .torrent while the client sits in
    // the tray) hands its argument to the running client and exits, so two
    // sessions never write the same files.
    if !desktop::claim_single_instance() { return Ok(()); }
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect(t("Не удалось запустить сетевой движок", "Could not start the network engine"));
    fs::create_dir_all(&settings.download_dir).ok();
    let persistence_dir = app_data_dir().join("session");
    fs::create_dir_all(&persistence_dir).ok();
    storage::register_persisted_folders(&persistence_dir.join("session.json"));
    // librqbit adds session-wide trackers to every non-private torrent.
    let session_trackers = fallback_trackers(&settings).into_iter().filter_map(|tracker| tracker.parse::<url::Url>().ok()).collect();
    let session_options = SessionOptions {
        ipv4_only: true,
        fastresume: true,
        listen: Some(ListenerOptions {
            mode: ListenerMode::TcpAndUtp,
            enable_upnp_port_forwarding: settings.enable_upnp_port_forwarding,
            ipv4_only: true,
            ..Default::default()
        }),
        trackers: session_trackers,
        persistence: Some(SessionPersistenceConfig::Json { folder: Some(persistence_dir) }),
        default_storage_factory: Some(librqbit::storage::StorageFactoryExt::boxed(storage::GentleStorageFactory)),
        ..Default::default()
    };
    let session = runtime.block_on(Session::new_with_opts(PathBuf::from(&settings.download_dir), session_options))
        .expect(t("Не удалось создать торрент-сессию", "Could not create the torrent session"));
    let icon_rgba = include_bytes!("../assets/cat-128.rgba").to_vec();
    let cat_icon = egui::IconData {
        rgba: icon_rgba.clone(),
        width: 128,
        height: 128,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(Arc::new(cat_icon))
            .with_inner_size([850.0, 560.0])
            .with_min_inner_size([360.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(app_name(), options, Box::new(move |cc| {
        cc.egui_ctx.set_visuals(cartoon_visuals());
        desktop::set_main_window(cc);
        let tray = desktop::Tray::new(&cc.egui_ctx, icon_rgba, 128);
        desktop::watch_inbox(cc.egui_ctx.clone());
        Ok(Box::new(TorrentClientApp::new(session, runtime, tray)))
    }))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magnet_display_name_is_decoded() {
        let uri = "magnet:?dn=My%20Movie+2024&xt=urn:btih:0123456789abcdef0123456789abcdef01234567&tr=udp%3A%2F%2Fexample.org%3A80";
        let params = magnet_params(uri);
        assert!(params.contains(&("dn".into(), "My Movie 2024".into())));
        assert!(params.iter().any(|(k, v)| k == "tr" && v == "udp://example.org:80"));
    }

    #[test]
    fn folder_name_is_sanitized() {
        assert_eq!(safe_folder_name("a/b:c?. "), "a_b_c_");
        assert_eq!(safe_folder_name("  "), "Загрузка");
    }

    #[test]
    fn tracker_list_is_parsed_and_validated() {
        let parsed = parse_tracker_list("udp://a.example:1337/announce

# comment
 https://b.example/announce 
udp://a.example:1337/announce").unwrap();
        assert_eq!(parsed, vec!["udp://a.example:1337/announce", "https://b.example/announce"]);
        assert!(parse_tracker_list("ftp://c.example/announce").is_err());
        assert!(parse_tracker_list("просто текст").is_err());
    }

    #[test]
    fn custom_trackers_follow_builtin_without_duplicates() {
        let mut settings = Settings { custom_trackers: vec!["udp://tracker.opentrackr.org:1337/announce".into(), "udp://mine.example:1/announce".into()], ..Settings::default() };
        let all = fallback_trackers(&settings);
        assert_eq!(all.len(), public_fallback_trackers().len() + 1);
        assert_eq!(all.last().unwrap(), "udp://mine.example:1/announce");
        settings.add_public_trackers = false;
        assert_eq!(fallback_trackers(&settings).len(), 2);
        assert!(!public_fallback_trackers().iter().any(|t| t.contains("pirates")));
    }
}
