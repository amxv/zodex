#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
compile_error!("zodex-tray is only for Windows and Linux; macOS uses the Swift menu bar app");

mod commands;
mod lifecycle;
mod platform;

use commands::CommandResult;
use lifecycle::{Lifecycle, Operation, RuntimeState};
use std::{
    env,
    path::{Path, PathBuf},
    process,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};
use tray_icon::{
    Icon, TrayIconBuilder, TrayIconEvent,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

// A tray that is supposed to report the truth must reconcile independently
// of Start/Stop command completion, including when a CLI is slow or wedged.
const STATUS_INTERVAL: Duration = Duration::from_secs(2);
const TRANSITION_STATUS_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
enum Task {
    Status,
    Start(PathBuf),
    Stop,
    OpenLiveboard,
    CopyLiveboard,
    CheckUpgrade,
    Upgrade(String, bool),
}

enum Response {
    Status {
        epoch: u64,
        result: CommandResult,
    },
    Finished {
        task: Task,
        id: u64,
        result: CommandResult,
    },
    FolderSelected(Result<Option<PathBuf>, String>),
}

struct MenuItems {
    start: MenuItem,
    stop: MenuItem,
    liveboard: MenuItem,
    copy: MenuItem,
    folder_label: MenuItem,
    folder_change: MenuItem,
    update: MenuItem,
    launch_at_login: CheckMenuItem,
    version: MenuItem,
    quit: MenuItem,
}

impl MenuItems {
    fn build() -> Result<(Self, Menu), String> {
        let menu = Menu::new();
        let items = Self {
            start: MenuItem::new("Start Zodex", false, None),
            stop: MenuItem::new("Stop Zodex", false, None),
            liveboard: MenuItem::new("Open Liveboard", false, None),
            copy: MenuItem::new("Copy Liveboard Link", false, None),
            folder_label: MenuItem::new("Start Folder: Not set", false, None),
            folder_change: MenuItem::new("Set Start Folder…", true, None),
            update: MenuItem::new("Check for Updates…", true, None),
            launch_at_login: CheckMenuItem::new(
                "Launch at Login",
                true,
                platform::launch_at_login_enabled(),
                None,
            ),
            version: MenuItem::new(
                format!("Zodex v{}", env!("ZODEX_OPERATOR_VERSION")),
                false,
                None,
            ),
            quit: MenuItem::new("Quit", true, None),
        };
        let divider1 = PredefinedMenuItem::separator();
        let divider2 = PredefinedMenuItem::separator();
        let divider3 = PredefinedMenuItem::separator();
        menu.append_items(&[
            &items.start,
            &items.stop,
            &items.liveboard,
            &items.copy,
            &divider1,
            &items.folder_label,
            &items.folder_change,
            &divider2,
            &items.update,
            &items.launch_at_login,
            &items.version,
            &divider3,
            &items.quit,
        ])
        .map_err(|e| e.to_string())?;
        Ok((items, menu))
    }
}

struct App {
    binary: PathBuf,
    tray: tray_icon::TrayIcon,
    items: MenuItems,
    preferences: platform::Preferences,
    sender: Sender<Response>,
    lifecycle: Lifecycle,
    active_command: Option<u64>,
    next_command_id: u64,
    status_epoch: u64,
    status_in_flight: bool,
    update_in_flight: bool,
    available_update: Option<String>,
    last_status: Instant,
}

impl App {
    fn new(
        binary: PathBuf,
        tray: tray_icon::TrayIcon,
        items: MenuItems,
        sender: Sender<Response>,
    ) -> Self {
        let mut app = Self {
            binary,
            tray,
            items,
            preferences: platform::preferences(),
            sender,
            lifecycle: Lifecycle::default(),
            active_command: None,
            next_command_id: 0,
            status_epoch: 0,
            status_in_flight: false,
            update_in_flight: false,
            available_update: None,
            last_status: Instant::now(),
        };
        app.refresh_folder();
        app.render_status();
        app.submit(Task::Status);
        app
    }

    fn refresh_folder(&mut self) {
        let value = match self.preferences.start_folder.as_deref() {
            Some(path) if path.is_dir() => format!("Start Folder: {}", abbreviated_path(path)),
            Some(path) => format!("Start Folder: {} (Unavailable)", abbreviated_path(path)),
            None => "Start Folder: Not set".to_owned(),
        };
        self.items.folder_label.set_text(value);
        self.items
            .folder_change
            .set_text(if self.preferences.start_folder.is_some() {
                "Change Start Folder…"
            } else {
                "Set Start Folder…"
            });
    }

    fn submit(&mut self, task: Task) {
        let mut id = 0;
        let mut epoch = self.status_epoch;
        match &task {
            Task::Status if self.status_in_flight => return,
            Task::Status => {
                self.status_in_flight = true;
                self.last_status = Instant::now();
            }
            Task::CheckUpgrade | Task::Upgrade(_, _) if self.update_in_flight => return,
            Task::CheckUpgrade | Task::Upgrade(_, _) => {
                self.update_in_flight = true;
                self.items.update.set_enabled(false);
                self.items.update.set_text("Working on update…");
            }
            _ if self.active_command.is_some() => return,
            _ => {
                self.next_command_id += 1;
                id = self.next_command_id;
                self.active_command = Some(id);
                match &task {
                    Task::Start(_) => self.lifecycle.begin(id, Operation::Start),
                    Task::Stop => self.lifecycle.begin(id, Operation::Stop),
                    _ => {}
                }
                // Ignore status results initiated before the requested
                // transition. They'll be stale as soon as Start/Stop begins.
                self.status_epoch += 1;
                epoch = self.status_epoch;
                self.render_status();
            }
        }
        let executable = self.binary.clone();
        let sender = self.sender.clone();
        thread::spawn(move || {
            let (args, start) = match &task {
                Task::Status => (
                    vec!["local".into(), "status".into(), "--json".into()],
                    false,
                ),
                Task::Start(path) => (
                    vec![
                        "local".into(),
                        "start".into(),
                        path.to_string_lossy().into_owned(),
                    ],
                    true,
                ),
                Task::Stop => (vec!["local".into(), "stop".into()], false),
                Task::OpenLiveboard => (vec!["local".into(), "watch".into()], false),
                Task::CopyLiveboard => (
                    vec!["local".into(), "watch".into(), "copyurl".into()],
                    false,
                ),
                Task::CheckUpgrade => (
                    vec![
                        "upgrade".into(),
                        "--check".into(),
                        "--format".into(),
                        "json".into(),
                    ],
                    false,
                ),
                Task::Upgrade(version, stop) => {
                    let mut args = vec![
                        "upgrade".into(),
                        "--version".into(),
                        version.clone(),
                        "--format".into(),
                        "json".into(),
                    ];
                    if *stop {
                        args.push("--stop-local".into());
                    }
                    (args, false)
                }
            };
            let argv = args.iter().map(String::as_str).collect::<Vec<_>>();
            let result = commands::run(&executable, &argv, start);
            let response = if matches!(task, Task::Status) {
                Response::Status { epoch, result }
            } else {
                Response::Finished { task, id, result }
            };
            let _ = sender.send(response);
        });
    }

    fn click(&mut self, event: MenuEvent) -> bool {
        let id = &event.id;
        if id == self.items.quit.id() {
            return false;
        } else if id == self.items.start.id() {
            if let Some(path) = self
                .preferences
                .start_folder
                .clone()
                .filter(|path| path.is_dir())
            {
                self.submit(Task::Start(path));
            } else {
                platform::show_message(
                    "Start folder unavailable",
                    "Choose an existing folder before starting Zodex.",
                    false,
                );
            }
        } else if id == self.items.stop.id() {
            self.submit(Task::Stop);
        } else if id == self.items.liveboard.id() {
            self.submit(Task::OpenLiveboard);
        } else if id == self.items.copy.id() {
            self.submit(Task::CopyLiveboard);
        } else if id == self.items.folder_change.id() {
            let path = self.preferences.start_folder.clone();
            let sender = self.sender.clone();
            thread::spawn(move || {
                let selection = platform::choose_folder(path.as_deref());
                let _ = sender.send(Response::FolderSelected(selection));
            });
        } else if id == self.items.launch_at_login.id() {
            let desired = !platform::launch_at_login_enabled();
            if let Err(error) = platform::set_launch_at_login(desired) {
                platform::show_message("Launch at Login could not be changed", &error, false);
            }
            self.items
                .launch_at_login
                .set_checked(platform::launch_at_login_enabled());
        } else if id == self.items.update.id() {
            if let Some(version) = &self.available_update {
                self.submit(Task::Upgrade(version.clone(), false));
            } else {
                self.submit(Task::CheckUpgrade);
            }
        }
        true
    }

    fn received(&mut self, response: Response) {
        match response {
            Response::FolderSelected(Ok(Some(path))) => {
                if !path.is_dir() {
                    platform::show_message(
                        "Start folder unavailable",
                        "The selected folder no longer exists.",
                        false,
                    );
                } else {
                    self.preferences.start_folder = Some(path.canonicalize().unwrap_or(path));
                    if let Err(error) = platform::save_preferences(&self.preferences) {
                        platform::show_message("Could not save Start Folder", &error, false);
                    }
                    self.refresh_folder();
                    self.render_status();
                    self.submit(Task::Status);
                }
            }
            Response::FolderSelected(Err(error)) => {
                platform::show_message("Could not choose Start Folder", &error, false);
            }
            Response::FolderSelected(Ok(None)) => {}
            Response::Status { epoch, result } => {
                self.status_in_flight = false;
                if epoch == self.status_epoch {
                    let state = commands::status(&result)
                        .map(|status| RuntimeState::parse(&status.state))
                        .unwrap_or(RuntimeState::Unavailable);
                    if let Some(id) = self.lifecycle.observe(state)
                        && self.active_command == Some(id)
                    {
                        self.active_command = None;
                    }
                    self.render_status();
                } else {
                    // An in-flight pre-click status response was stale;
                    // schedule a fresh observation on the next event tick.
                    self.last_status = Instant::now() - STATUS_INTERVAL;
                }
            }
            Response::Finished { task, id, result } => match task {
                Task::Status => unreachable!("status has its own response type"),
                Task::CheckUpgrade | Task::Upgrade(_, _) => {
                    self.update_in_flight = false;
                    self.items.update.set_enabled(true);
                    self.apply_upgrade(task, &result);
                }
                _ => {
                    if self.active_command != Some(id) {
                        // Polling already observed the true target state, or
                        // this is an old completion from before a new click.
                        return;
                    }
                    self.active_command = None;
                    self.lifecycle.finish(id);
                    self.status_epoch += 1;
                    self.render_status();
                    if !result.ok {
                        platform::show_message("Zodex command failed", &result.output, false);
                    } else if matches!(task, Task::CopyLiveboard) {
                        self.items.copy.set_text("Liveboard Link Copied");
                    }
                    self.last_status = Instant::now() - STATUS_INTERVAL;
                    self.submit(Task::Status);
                }
            },
        }
    }

    fn render_status(&self) {
        let start_folder_valid = self
            .preferences
            .start_folder
            .as_deref()
            .is_some_and(Path::is_dir);
        let ui = self
            .lifecycle
            .presentation(start_folder_valid, self.active_command.is_some());
        self.items.start.set_text(ui.label);
        self.items.start.set_enabled(ui.start_enabled);
        self.items.stop.set_enabled(ui.stop_enabled);
        self.items.liveboard.set_enabled(ui.liveboard_enabled);
        self.items.copy.set_enabled(ui.liveboard_enabled);
        let _ = self.tray.set_tooltip(Some(ui.tooltip));
    }

    fn apply_upgrade(&mut self, task: Task, result: &CommandResult) {
        match task {
            Task::CheckUpgrade => {
                if let Some(event) = commands::upgrade_event(result, "check_complete") {
                    self.items
                        .version
                        .set_text(format!("Zodex v{}", event.current_version));
                    self.available_update = event
                        .target_version
                        .filter(|_| event.update_available == Some(true));
                    if self.available_update.is_none() {
                        platform::show_message("Zodex is up to date", &event.message, false);
                    }
                } else {
                    platform::show_message("Update check failed", &result.output, false);
                }
            }
            Task::Upgrade(version, false)
                if !result.ok
                    && commands::upgrade_event(result, "failed")
                        .as_ref()
                        .and_then(|event| event.code.as_deref())
                        == Some("local_running") =>
            {
                if platform::show_message(
                    "Stop Zodex Local and update?",
                    "The update requires stopping the Local runtime. Active Zodex processes will be stopped.",
                    true,
                ) {
                    self.submit(Task::Upgrade(version, true));
                    return;
                }
            }
            Task::Upgrade(_, _) if result.ok => {
                self.available_update = None;
                platform::show_message(
                    "Zodex update initiated",
                    "The update will complete shortly. The tray may restart as the installed files are replaced.",
                    false,
                );
            }
            Task::Upgrade(_, _) => {
                platform::show_message(
                    "Zodex update failed",
                    &commands::upgrade_event(result, "failed")
                        .map(|event| event.message)
                        .unwrap_or_else(|| result.output.clone()),
                    false,
                );
            }
            _ => {}
        }
        self.items
            .update
            .set_text(self.available_update.as_ref().map_or_else(
                || "Check for Updates…".to_owned(),
                |version| format!("Update to v{version}…"),
            ));
    }
}

fn abbreviated_path(path: &Path) -> String {
    let path = path.to_string_lossy().into_owned();
    // Windows canonicalize() returns verbatim paths (\\?\C:\Users\...).
    // Keep those for subprocesses, but never show raw API prefixes in menus.
    #[cfg(target_os = "windows")]
    let path = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
    };
    if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        let home = home.to_string_lossy();
        if path == home {
            return "~".to_owned();
        }
        if let Some(rest) = path.strip_prefix(&format!("{home}{}", std::path::MAIN_SEPARATOR)) {
            return format!("~{}{}", std::path::MAIN_SEPARATOR, rest);
        }
    }
    path
}

/// Match macOS's plain semibold "Z" menu-bar text: no square, tile or
/// background. Supersampling keeps the monochrome glyph clean at tray sizes.
fn icon_rgba(foreground: [u8; 3]) -> Vec<u8> {
    // The Z silhouette comes from the Zodex favicon with the background
    // removed, centered and scaled to a 22 px glyph in a 32 px tray icon.
    let polygon = [
        (24.0, 28.0),
        (72.0, 28.0),
        (72.0, 35.0),
        (39.0, 68.0),
        (72.0, 68.0),
        (72.0, 76.0),
        (23.0, 76.0),
        (23.0, 69.0),
        (56.0, 36.0),
        (24.0, 36.0),
    ];
    fn inside(x: f64, y: f64, polygon: &[(f64, f64)]) -> bool {
        let mut hit = false;
        let mut j = polygon.len() - 1;
        for i in 0..polygon.len() {
            let (ix, iy) = polygon[i];
            let (jx, jy) = polygon[j];
            if (iy > y) != (jy > y) && x < (jx - ix) * (y - iy) / (jy - iy) + ix {
                hit = !hit;
            }
            j = i;
        }
        hit
    }
    let mut rgba = vec![0; 32 * 32 * 4];
    for y in 0..32 {
        for x in 0..32 {
            let mut coverage = 0u8;
            for sy in 0..4 {
                for sx in 0..4 {
                    let point_x = ((x * 4 + sx) as f64 / 4.0 + 0.125 - 5.0) / 0.44 + 23.0;
                    let point_y = ((y * 4 + sy) as f64 / 4.0 + 0.125 - 5.0) / 0.44 + 28.0;
                    coverage += u8::from(inside(point_x, point_y, &polygon));
                }
            }
            let offset = (y * 32 + x) * 4;
            rgba[offset..offset + 3].copy_from_slice(&foreground);
            rgba[offset + 3] = (u16::from(coverage) * 255 / 16) as u8;
        }
    }
    rgba
}

fn zodex_icon() -> Result<Icon, String> {
    Icon::from_rgba(icon_rgba(platform::tray_icon_foreground()), 32, 32).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_icon_has_no_tile_or_background_and_has_a_centered_z() {
        let rgba = icon_rgba([247, 247, 247]);
        assert_eq!(rgba.len(), 32 * 32 * 4);
        for (x, y) in [(0, 0), (31, 31), (0, 16), (16, 0), (16, 31)] {
            assert_eq!(
                rgba[(y * 32 + x) * 4 + 3],
                0,
                "background must be transparent"
            );
        }
        assert_eq!(rgba[(8 * 32 + 16) * 4 + 3], 255, "top bar of Z");
        assert_eq!(rgba[(16 * 32 + 16) * 4 + 3], 255, "diagonal of Z");
        assert_eq!(rgba[(24 * 32 + 16) * 4 + 3], 255, "bottom bar of Z");
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() == 1 {
        match args[0].as_str() {
            "--enable-autostart" => return platform::set_launch_at_login(true),
            "--disable-autostart" => return platform::set_launch_at_login(false),
            "--autostart-status" => {
                println!(
                    "{}",
                    if platform::launch_at_login_enabled() {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
                return Ok(());
            }
            "--version" => {
                println!("zodex-tray {}", env!("ZODEX_OPERATOR_VERSION"));
                return Ok(());
            }
            _ => {}
        }
    }
    if !args.is_empty() {
        return Err("Usage: zodex-tray [--enable-autostart|--disable-autostart|--autostart-status|--version]".into());
    }
    #[cfg(target_os = "linux")]
    if !platform::graphical_session() {
        return Err("Zodex tray needs a Linux desktop session (DISPLAY or WAYLAND_DISPLAY); the Local CLI also works headlessly.".into());
    }
    let Some(_lock) = platform::single_instance()? else {
        return Ok(());
    };
    let binary = platform::zodex_executable()?;
    let (items, menu) = MenuItems::build()?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("Zodex Local")
        .with_icon(zodex_icon()?)
        .with_menu(Box::new(menu))
        .build()
        .map_err(|e| format!("Could not create system tray icon: {e}"))?;
    let (sender, receiver): (Sender<Response>, Receiver<Response>) = mpsc::channel();
    let mut app = App::new(binary, tray, items, sender);
    loop {
        platform::pump_events();
        // Reconcile as soon as the user opens the tray, rather than waiting
        // for the next background poll.
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if matches!(
                event,
                TrayIconEvent::Click { .. } | TrayIconEvent::DoubleClick { .. }
            ) {
                app.submit(Task::Status);
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if !app.click(event) {
                return Ok(());
            }
        }
        while let Ok(response) = receiver.try_recv() {
            app.received(response);
        }
        let interval = if app.lifecycle.pending() {
            TRANSITION_STATUS_INTERVAL
        } else {
            STATUS_INTERVAL
        };
        if app.last_status.elapsed() >= interval && !app.status_in_flight {
            app.submit(Task::Status);
        }
        thread::sleep(Duration::from_millis(80));
    }
}

fn main() {
    if let Err(error) = run() {
        platform::show_message("Zodex tray error", &error, false);
        eprintln!("zodex-tray: {error}");
        process::exit(1);
    }
}
