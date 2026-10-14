#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
compile_error!("zodex-tray is only for Windows and Linux; macOS uses the Swift menu bar app");

mod commands;
mod platform;

use commands::CommandResult;
use std::{
    env,
    path::{Path, PathBuf},
    process,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

const STATUS_INTERVAL: Duration = Duration::from_secs(8);

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
    Finished(Task, CommandResult),
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

    fn runtime_disabled(&self) {
        self.start.set_enabled(false);
        self.stop.set_enabled(false);
        self.liveboard.set_enabled(false);
        self.copy.set_enabled(false);
    }
}

struct App {
    binary: PathBuf,
    items: MenuItems,
    preferences: platform::Preferences,
    sender: Sender<Response>,
    in_flight: bool,
    status_in_flight: bool,
    update_in_flight: bool,
    available_update: Option<String>,
    last_status: Instant,
}

impl App {
    fn new(binary: PathBuf, items: MenuItems, sender: Sender<Response>) -> Self {
        let mut app = Self {
            binary,
            items,
            preferences: platform::preferences(),
            sender,
            in_flight: false,
            status_in_flight: false,
            update_in_flight: false,
            available_update: None,
            last_status: Instant::now(),
        };
        app.refresh_folder();
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
        match task {
            Task::Status if self.status_in_flight || self.in_flight => return,
            Task::Status => self.status_in_flight = true,
            Task::CheckUpgrade | Task::Upgrade(_, _) if self.update_in_flight => return,
            Task::CheckUpgrade | Task::Upgrade(_, _) => {
                self.update_in_flight = true;
                self.items.update.set_enabled(false);
                self.items.update.set_text("Working on update…");
            }
            _ if self.in_flight => return,
            _ => {
                self.in_flight = true;
                self.items.runtime_disabled();
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
            let _ = sender.send(Response::Finished(task, result));
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
                self.items.start.set_text("Starting Zodex…");
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
                    self.submit(Task::Status);
                }
            }
            Response::FolderSelected(Err(error)) => {
                platform::show_message("Could not choose Start Folder", &error, false);
            }
            Response::FolderSelected(Ok(None)) => {}
            Response::Finished(task, result) => match task {
                Task::Status => {
                    self.status_in_flight = false;
                    self.last_status = Instant::now();
                    self.apply_status(&result);
                }
                Task::CheckUpgrade | Task::Upgrade(_, _) => {
                    self.update_in_flight = false;
                    self.items.update.set_enabled(true);
                    self.apply_upgrade(task, &result);
                }
                _ => {
                    self.in_flight = false;
                    if !result.ok {
                        platform::show_message("Zodex command failed", &result.output, false);
                    } else if matches!(task, Task::CopyLiveboard) {
                        self.items.copy.set_text("Liveboard Link Copied");
                    }
                    self.submit(Task::Status);
                }
            },
        }
    }

    fn apply_status(&self, result: &CommandResult) {
        self.items.start.set_text("Start Zodex");
        self.items.copy.set_text("Copy Liveboard Link");
        if self.in_flight {
            return;
        }
        let start_folder_valid = self
            .preferences
            .start_folder
            .as_deref()
            .is_some_and(Path::is_dir);
        match commands::status(result)
            .as_ref()
            .map(|status| status.state.as_str())
        {
            Some("running") => {
                self.items.start.set_text("Zodex is Running");
                self.items.start.set_enabled(false);
                self.items.stop.set_enabled(true);
                self.items.liveboard.set_enabled(true);
                self.items.copy.set_enabled(true);
            }
            Some("stale") => {
                self.items.runtime_disabled();
                self.items.stop.set_enabled(true);
            }
            Some("stopped") => {
                self.items.runtime_disabled();
                self.items.start.set_enabled(start_folder_valid);
            }
            Some("unconfigured") => {
                self.items.runtime_disabled();
                self.items.start.set_text("Run zodex local setup first");
            }
            _ => {
                self.items.runtime_disabled();
                self.items.start.set_enabled(start_folder_valid);
                self.items.stop.set_enabled(true);
            }
        }
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
    let path = path.to_string_lossy();
    if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        let home = home.to_string_lossy();
        if path == home {
            return "~".to_owned();
        }
        if let Some(rest) = path.strip_prefix(&format!("{home}{}", std::path::MAIN_SEPARATOR)) {
            return format!("~{}{}", std::path::MAIN_SEPARATOR, rest);
        }
    }
    path.into_owned()
}

fn zodex_icon() -> Result<Icon, String> {
    let mut rgba = vec![0; 32 * 32 * 4];
    let z = [
        0b11111u8, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
    ];
    for y in 0usize..32 {
        for x in 0usize..32 {
            let corner_x = if x < 7 { 7 - x } else { x.saturating_sub(24) };
            let corner_y = if y < 7 { 7 - y } else { y.saturating_sub(24) };
            let rounded = corner_x * corner_x + corner_y * corner_y < 49;
            let pixel = (y * 32 + x) * 4;
            if rounded {
                let glyph_x = (x as isize - 6) / 4;
                let glyph_y = (y as isize - 4) / 3;
                let white = (0..5).contains(&glyph_x)
                    && (0..7).contains(&glyph_y)
                    && z[glyph_y as usize] & (1 << (4 - glyph_x)) != 0;
                rgba[pixel..pixel + 4].copy_from_slice(if white {
                    &[255, 255, 255, 255]
                } else {
                    &[35, 137, 194, 255]
                });
            }
        }
    }
    Icon::from_rgba(rgba, 32, 32).map_err(|e| e.to_string())
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
    let _tray = TrayIconBuilder::new()
        .with_tooltip("Zodex Local")
        .with_icon(zodex_icon()?)
        .with_menu(Box::new(menu))
        .build()
        .map_err(|e| format!("Could not create system tray icon: {e}"))?;
    let (sender, receiver): (Sender<Response>, Receiver<Response>) = mpsc::channel();
    let mut app = App::new(binary, items, sender);
    loop {
        platform::pump_events();
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if !app.click(event) {
                return Ok(());
            }
        }
        while let Ok(response) = receiver.try_recv() {
            app.received(response);
        }
        if app.last_status.elapsed() >= STATUS_INTERVAL && !app.status_in_flight {
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
