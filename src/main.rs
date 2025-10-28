use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Tabs},
    Terminal,
};
use std::{
    env,
    io::{self, BufRead, BufReader},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Browser,
    Downloads,
    History,
}

#[derive(Clone, Copy, PartialEq)]
enum InputMode {
    Normal,
    EditingPath,
    Filtering,
}

#[derive(Clone, PartialEq)]
enum DownloadStatus {
    Queued,
    Downloading,
    Completed,
    Failed(String),
}

#[derive(Clone)]
struct FolderInfo {
    name: String,
}

#[derive(Clone)]
struct Download {
    id: u64,
    folder_name: String,
    remote_path: String,
    status: DownloadStatus,
    started_at: Option<u64>,
    completed_at: Option<u64>,
}

#[derive(Clone)]
struct HistoryEntry {
    folder_name: String,
    remote_path: String,
    downloaded_at: u64,
}

#[derive(Clone)]
struct DownloadProgress {
    file_name: String,
    percentage: u16,
    speed: String,
}

struct App {
    remote_host: String,
    remote_base_path: String,
    current_path: String,
    local_dest: String,

    // Tab navigation
    current_tab: Tab,

    // Input handling
    input_mode: InputMode,
    input_buffer: String,

    // Browser tab
    folders: Vec<FolderInfo>,
    all_folders: Vec<FolderInfo>, // Unfiltered list
    browser_list_state: ListState,
    filter_query: String,
    saved_filter_query: String, // Filter state before entering filter mode

    // Downloads tab
    downloads: Arc<Mutex<Vec<Download>>>,
    downloads_list_state: ListState,
    next_download_id: u64,
    active_download_info: Arc<Mutex<Option<DownloadProgress>>>,

    // History tab
    history: Vec<HistoryEntry>,
    history_list_state: ListState,

    status_message: String,
}

impl App {
    fn new(remote_source: String, local_dest: String) -> io::Result<Self> {
        // Parse remote_source into host and path
        let (remote_host, remote_base_path) = if let Some((host, path)) = remote_source.split_once(':') {
            (host.to_string(), path.to_string())
        } else {
            (remote_source.clone(), String::new())
        };

        let current_path = remote_base_path.clone();
        let mut folders = list_remote_folders(&remote_host, &current_path)?;
        folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        let mut browser_list_state = ListState::default();
        if !folders.is_empty() {
            browser_list_state.select(Some(0));
        }

        Ok(App {
            remote_host,
            remote_base_path,
            current_path,
            local_dest,
            current_tab: Tab::Browser,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            all_folders: folders.clone(),
            folders,
            browser_list_state,
            filter_query: String::new(),
            saved_filter_query: String::new(),
            downloads: Arc::new(Mutex::new(Vec::new())),
            downloads_list_state: ListState::default(),
            next_download_id: 1,
            active_download_info: Arc::new(Mutex::new(None)),
            history: Vec::new(),
            history_list_state: ListState::default(),
            status_message: String::new(),
        })
    }

    fn next_tab(&mut self) {
        self.current_tab = match self.current_tab {
            Tab::Browser => Tab::Downloads,
            Tab::Downloads => Tab::History,
            Tab::History => Tab::Browser,
        };
    }

    fn prev_tab(&mut self) {
        self.current_tab = match self.current_tab {
            Tab::Browser => Tab::History,
            Tab::Downloads => Tab::Browser,
            Tab::History => Tab::Downloads,
        };
    }

    fn start_filtering(&mut self) {
        if self.current_tab != Tab::Browser {
            return;
        }
        // Save current filter state before entering filter mode
        self.saved_filter_query = self.filter_query.clone();
        self.input_mode = InputMode::Filtering;
        self.input_buffer = self.filter_query.clone();
        self.status_message = "Filter (Enter: confirm, Esc: cancel)".to_string();
    }

    fn apply_filter(&mut self) {
        use fuzzy_matcher::FuzzyMatcher;
        use fuzzy_matcher::skim::SkimMatcherV2;

        if self.filter_query.is_empty() {
            self.folders = self.all_folders.clone();
        } else {
            let matcher = SkimMatcherV2::default();
            let mut scored_folders: Vec<(i64, FolderInfo)> = self.all_folders
                .iter()
                .filter_map(|folder| {
                    matcher.fuzzy_match(&folder.name, &self.filter_query)
                        .map(|score| (score, folder.clone()))
                })
                .collect();

            scored_folders.sort_by(|a, b| b.0.cmp(&a.0));
            self.folders = scored_folders.into_iter().map(|(_, f)| f).collect();
        }

        // Reset selection
        self.browser_list_state.select(if self.folders.is_empty() { None } else { Some(0) });
    }

    fn confirm_filter(&mut self) {
        // filter_query is already set by real-time typing, just exit mode
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();

        let msg = if self.filter_query.is_empty() {
            "Filter cleared".to_string()
        } else {
            format!("Filter: {} ({} results)", self.filter_query, self.folders.len())
        };
        self.status_message = msg;
    }

    fn cancel_filter(&mut self) {
        // Restore previous filter state
        self.filter_query = self.saved_filter_query.clone();
        self.apply_filter();
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();

        let msg = if self.filter_query.is_empty() {
            "Filter cancelled".to_string()
        } else {
            format!("Filter restored: {} ({} results)", self.filter_query, self.folders.len())
        };
        self.status_message = msg;
    }

    fn page_up(&mut self) {
        let (list_state, len) = match self.current_tab {
            Tab::Browser => (&mut self.browser_list_state, self.folders.len()),
            Tab::Downloads => (&mut self.downloads_list_state, self.downloads.lock().unwrap().len()),
            Tab::History => (&mut self.history_list_state, self.history.len()),
        };

        if len == 0 {
            return;
        }

        let page_size = 10;
        let current = list_state.selected().unwrap_or(0);
        let new_pos = current.saturating_sub(page_size);
        list_state.select(Some(new_pos));
    }

    fn page_down(&mut self) {
        let (list_state, len) = match self.current_tab {
            Tab::Browser => (&mut self.browser_list_state, self.folders.len()),
            Tab::Downloads => (&mut self.downloads_list_state, self.downloads.lock().unwrap().len()),
            Tab::History => (&mut self.history_list_state, self.history.len()),
        };

        if len == 0 {
            return;
        }

        let page_size = 10;
        let current = list_state.selected().unwrap_or(0);
        let new_pos = std::cmp::min(current + page_size, len - 1);
        list_state.select(Some(new_pos));
    }

    fn start_editing_path(&mut self) {
        self.input_mode = InputMode::EditingPath;
        self.input_buffer = self.local_dest.clone();
        self.status_message = "Editing download destination (Enter: save, Esc: cancel)".to_string();
    }

    fn cancel_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.status_message = "Cancelled".to_string();
    }

    fn confirm_path_change(&mut self) {
        if !self.input_buffer.is_empty() {
            self.local_dest = self.input_buffer.clone();
            self.status_message = format!("Download destination changed to: {}", self.local_dest);
        }
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    fn handle_input_char(&mut self, c: char) {
        match self.input_mode {
            InputMode::EditingPath => {
                self.input_buffer.push(c);
            }
            InputMode::Filtering => {
                self.input_buffer.push(c);
                self.filter_query = self.input_buffer.clone();
                self.apply_filter();
            }
            InputMode::Normal => {}
        }
    }

    fn handle_input_backspace(&mut self) {
        match self.input_mode {
            InputMode::EditingPath => {
                self.input_buffer.pop();
            }
            InputMode::Filtering => {
                self.input_buffer.pop();
                self.filter_query = self.input_buffer.clone();
                self.apply_filter();
            }
            InputMode::Normal => {}
        }
    }

    fn next(&mut self) {
        let (list_state, len) = match self.current_tab {
            Tab::Browser => (&mut self.browser_list_state, self.folders.len()),
            Tab::Downloads => (&mut self.downloads_list_state, self.downloads.lock().unwrap().len()),
            Tab::History => (&mut self.history_list_state, self.history.len()),
        };

        if len == 0 {
            return;
        }

        let i = match list_state.selected() {
            Some(i) => {
                if i >= len - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        list_state.select(Some(i));
    }

    fn previous(&mut self) {
        let (list_state, len) = match self.current_tab {
            Tab::Browser => (&mut self.browser_list_state, self.folders.len()),
            Tab::Downloads => (&mut self.downloads_list_state, self.downloads.lock().unwrap().len()),
            Tab::History => (&mut self.history_list_state, self.history.len()),
        };

        if len == 0 {
            return;
        }

        let i = match list_state.selected() {
            Some(i) => {
                if i == 0 {
                    len - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        list_state.select(Some(i));
    }

    fn enter_folder(&mut self) -> io::Result<()> {
        if self.current_tab != Tab::Browser {
            return Ok(());
        }

        if let Some(i) = self.browser_list_state.selected() {
            let folder = self.folders[i].name.clone();

            // Update current path
            self.current_path = if self.current_path.is_empty() {
                folder.clone()
            } else {
                format!("{}/{}", self.current_path, folder)
            };

            // List folders in the new path
            match list_remote_folders(&self.remote_host, &self.current_path) {
                Ok(mut folders) => {
                    folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                    self.all_folders = folders.clone();
                    self.filter_query.clear();
                    self.folders = folders;
                    self.browser_list_state.select(if self.folders.is_empty() { None } else { Some(0) });
                    self.status_message = format!("Entered: {}", folder);
                }
                Err(e) => {
                    // Revert path on error
                    let parts: Vec<&str> = self.current_path.rsplitn(2, '/').collect();
                    self.current_path = if parts.len() > 1 {
                        parts[1].to_string()
                    } else {
                        self.remote_base_path.clone()
                    };
                    self.status_message = format!("Error entering folder: {}", e);
                }
            }
        }
        Ok(())
    }

    fn go_back(&mut self) -> io::Result<()> {
        if self.current_tab != Tab::Browser {
            return Ok(());
        }

        // Check if we can go back
        if self.current_path == self.remote_base_path {
            self.status_message = "Already at base path".to_string();
            return Ok(());
        }

        // Go up one level
        let parts: Vec<&str> = self.current_path.rsplitn(2, '/').collect();
        self.current_path = if parts.len() > 1 {
            parts[1].to_string()
        } else {
            self.remote_base_path.clone()
        };

        // Refresh folder list
        match list_remote_folders(&self.remote_host, &self.current_path) {
            Ok(mut folders) => {
                folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                self.all_folders = folders.clone();
                self.filter_query.clear();
                self.folders = folders;
                self.browser_list_state.select(if self.folders.is_empty() { None } else { Some(0) });
                self.status_message = "Went back".to_string();
            }
            Err(e) => {
                self.status_message = format!("Error going back: {}", e);
            }
        }
        Ok(())
    }

    fn queue_download(&mut self) {
        if self.current_tab != Tab::Browser {
            return;
        }

        if let Some(i) = self.browser_list_state.selected() {
            let folder = &self.folders[i].name;

            let full_path = if self.current_path.is_empty() {
                folder.clone()
            } else {
                format!("{}/{}", self.current_path, folder)
            };

            let remote_path = format!("{}:{}", self.remote_host, full_path);
            let download = Download {
                id: self.next_download_id,
                folder_name: folder.clone(),
                remote_path: remote_path.clone(),
                status: DownloadStatus::Queued,
                started_at: None,
                completed_at: None,
            };

            self.next_download_id += 1;
            self.downloads.lock().unwrap().push(download);
            self.status_message = format!("Queued: {}", folder);

            // Start download worker if needed
            self.process_download_queue();
        }
    }

    fn process_download_queue(&self) {
        let downloads = Arc::clone(&self.downloads);
        let local_dest = self.local_dest.clone();
        let active_info = Arc::clone(&self.active_download_info);

        thread::spawn(move || {
            loop {
                let mut download_to_process = None;

                // Find next queued download
                {
                    let mut downloads_lock = downloads.lock().unwrap();
                    for download in downloads_lock.iter_mut() {
                        if download.status == DownloadStatus::Queued {
                            download.status = DownloadStatus::Downloading;
                            download.started_at = Some(
                                SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs(),
                            );
                            download_to_process = Some(download.clone());
                            break;
                        }
                    }
                }

                if let Some(download) = download_to_process {
                    // Run rsync with piped output and --info=progress2 for machine-readable progress
                    let mut child = Command::new("rsync")
                        .arg("-vrtzhP")
                        .arg("--info=progress2")
                        .arg(&download.remote_path)
                        .arg(&local_dest)
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn();

                    let success = if let Ok(ref mut child_process) = child {
                        // Spawn thread to read and parse stderr (where progress goes)
                        if let Some(stderr) = child_process.stderr.take() {
                            let info_clone = Arc::clone(&active_info);
                            thread::spawn(move || {
                                let reader = BufReader::new(stderr);
                                let mut current_file = String::new();

                                for line in reader.lines().flatten() {
                                    // Parse rsync output
                                    let parsed = parse_rsync_line(&line, &mut current_file);
                                    if let Some(info) = parsed {
                                        *info_clone.lock().unwrap() = Some(info);
                                    }
                                }
                            });
                        }

                        // Also read stdout to prevent blocking
                        if let Some(stdout) = child_process.stdout.take() {
                            let info_clone = Arc::clone(&active_info);
                            thread::spawn(move || {
                                let reader = BufReader::new(stdout);
                                let mut current_file = String::new();

                                for line in reader.lines().flatten() {
                                    // Parse rsync output
                                    let parsed = parse_rsync_line(&line, &mut current_file);
                                    if let Some(info) = parsed {
                                        *info_clone.lock().unwrap() = Some(info);
                                    }
                                }
                            });
                        }

                        // Wait for completion
                        child_process.wait().map(|status| status.success()).unwrap_or(false)
                    } else {
                        false
                    };

                    // Clear active download info
                    *active_info.lock().unwrap() = None;

                    // Update status
                    let mut downloads_lock = downloads.lock().unwrap();
                    if let Some(d) = downloads_lock.iter_mut().find(|d| d.id == download.id) {
                        if success {
                            d.status = DownloadStatus::Completed;
                            d.completed_at = Some(
                                SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs(),
                            );
                        } else {
                            d.status = DownloadStatus::Failed("rsync failed".to_string());
                            d.completed_at = Some(
                                SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs(),
                            );
                        }
                    }
                } else {
                    // No more queued downloads, exit thread
                    break;
                }
            }
        });
    }

    fn move_completed_to_history(&mut self) {
        let mut downloads = self.downloads.lock().unwrap();
        let mut to_remove = Vec::new();

        for (idx, download) in downloads.iter().enumerate() {
            if let DownloadStatus::Completed = download.status {
                if let Some(completed_at) = download.completed_at {
                    self.history.push(HistoryEntry {
                        folder_name: download.folder_name.clone(),
                        remote_path: download.remote_path.clone(),
                        downloaded_at: completed_at,
                    });
                    to_remove.push(idx);
                }
            }
        }

        // Remove from downloads in reverse order to maintain indices
        for idx in to_remove.iter().rev() {
            downloads.remove(*idx);
        }
    }

    fn clear_history_item(&mut self) {
        if self.current_tab != Tab::History {
            return;
        }

        if let Some(idx) = self.history_list_state.selected() {
            if idx < self.history.len() {
                let removed = self.history.remove(idx);
                self.status_message = format!("Removed: {}", removed.folder_name);

                // Adjust selection
                if self.history.is_empty() {
                    self.history_list_state.select(None);
                } else if idx >= self.history.len() {
                    self.history_list_state.select(Some(self.history.len() - 1));
                }
            }
        }
    }

    fn clear_all_history(&mut self) {
        if self.current_tab != Tab::History {
            return;
        }

        let count = self.history.len();
        self.history.clear();
        self.history_list_state.select(None);
        self.status_message = format!("Cleared {} history items", count);
    }
}

fn parse_rsync_line(line: &str, current_file: &mut String) -> Option<DownloadProgress> {
    let trimmed = line.trim();

    // Check if it's a progress line with speed (contains % and /s)
    // Format: "     1,234,567  45%    1.23MB/s    0:00:12"
    if trimmed.contains('%') && trimmed.contains("/s") {
        // Split by whitespace and find the speed component
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        let mut percentage = 0u16;
        let mut speed = String::new();

        for (i, part) in parts.iter().enumerate() {
            if part.contains("/s") {
                speed = part.to_string();
            }
            if part.ends_with('%') {
                // Parse percentage
                if let Ok(pct) = part.trim_end_matches('%').parse::<u16>() {
                    percentage = pct.min(100);
                }
            }
        }

        if !speed.is_empty() {
            let file_name = if !current_file.is_empty() {
                current_file.clone()
            } else {
                "Syncing...".to_string()
            };

            return Some(DownloadProgress {
                file_name,
                percentage,
                speed,
            });
        }
    }
    // Check if it's a file name line
    // File names don't start with whitespace and aren't rsync metadata
    else if !trimmed.is_empty()
        && !trimmed.starts_with(char::is_whitespace)
        && !trimmed.starts_with("receiving")
        && !trimmed.starts_with("sending")
        && !trimmed.starts_with("sent")
        && !trimmed.starts_with("total")
        && !trimmed.starts_with("building")
        && !trimmed.contains("speedup")
        && !trimmed.contains("bytes/sec")
        && trimmed.len() < 200  // Reasonable file name length
        && !trimmed.contains("to-check")
        && !trimmed.contains("to-chk")
    {
        // This looks like a file name - extract just the filename, not full path
        let file_path = std::path::Path::new(trimmed);
        if let Some(file_name) = file_path.file_name() {
            if let Some(name_str) = file_name.to_str() {
                *current_file = name_str.to_string();
            }
        }
    }

    None
}

fn list_remote_folders(remote_host: &str, remote_path: &str) -> io::Result<Vec<FolderInfo>> {
    let path = if remote_path.is_empty() { "." } else { remote_path };

    // List folders
    let output = Command::new("ssh")
        .arg(remote_host)
        .arg(format!(
            "find {} -maxdepth 1 -type d -not -path {}",
            path, path
        ))
        .output()?;

    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            String::from_utf8_lossy(&output.stderr),
        ));
    }

    let folders: Vec<FolderInfo> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }

            let name = std::path::Path::new(trimmed)
                .file_name()?
                .to_str()?
                .to_string();

            Some(FolderInfo { name })
        })
        .collect();

    Ok(folders)
}


fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        eprintln!("Usage: {} <remote_source> <local_dest>", args[0]);
        eprintln!("Example: {} user@hostname ./downloads", args[0]);
        eprintln!("Or with path: {} user@hostname:/path/to/folder ./downloads", args[0]);
        std::process::exit(1);
    }

    let remote_source = args[1].clone();
    let local_dest = args[2].clone();

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = App::new(remote_source, local_dest)?;

    // Run app
    let res = run_app(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("Error: {:?}", err);
    }

    Ok(())
}

fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> io::Result<()> {
    loop {
        // Move completed downloads to history
        app.move_completed_to_history();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Min(0),
                    Constraint::Length(3),
                ])
                .split(f.area());

            // Tab bar
            let tab_titles = vec!["Browser", "Downloads", "History"];
            let tabs = Tabs::new(tab_titles)
                .block(Block::default().borders(Borders::ALL).title("Lakach"))
                .select(match app.current_tab {
                    Tab::Browser => 0,
                    Tab::Downloads => 1,
                    Tab::History => 2,
                })
                .style(Style::default().fg(Color::White))
                .highlight_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
            f.render_widget(tabs, chunks[0]);

            // Title/info bar
            let title_text = match app.current_tab {
                Tab::Browser => {
                    let path = if app.current_path.is_empty() {
                        format!("{}:~", app.remote_host)
                    } else {
                        format!("{}:{}", app.remote_host, app.current_path)
                    };
                    if app.filter_query.is_empty() {
                        path
                    } else {
                        format!("{} | Filter: {}", path, app.filter_query)
                    }
                }
                Tab::Downloads => {
                    let downloads = app.downloads.lock().unwrap();
                    format!("Active: {} | Queued: {} | Total: {}",
                        downloads.iter().filter(|d| d.status == DownloadStatus::Downloading).count(),
                        downloads.iter().filter(|d| d.status == DownloadStatus::Queued).count(),
                        downloads.len())
                }
                Tab::History => format!("Downloaded this session: {}", app.history.len()),
            };
            let title = Paragraph::new(title_text)
                .style(Style::default().fg(Color::Cyan))
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(title, chunks[1]);

            // Split main area for content and legend
            let main_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Min(0),
                    Constraint::Length(20),
                ])
                .split(chunks[2]);

            // Main content
            match app.current_tab {
                Tab::Browser => {
                    let items: Vec<ListItem> = app
                        .folders
                        .iter()
                        .map(|folder| ListItem::new(folder.name.as_str()))
                        .collect();

                    let list = List::new(items)
                        .block(Block::default().borders(Borders::ALL).title("Folders"))
                        .highlight_style(
                            Style::default()
                                .bg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD),
                        )
                        .highlight_symbol(">> ");

                    f.render_stateful_widget(list, main_chunks[0], &mut app.browser_list_state);
                }
                Tab::Downloads => {
                    let downloads = app.downloads.lock().unwrap();
                    let items: Vec<ListItem> = downloads
                        .iter()
                        .map(|d| {
                            let status_str = match &d.status {
                                DownloadStatus::Queued => "Queued".to_string(),
                                DownloadStatus::Downloading => "Downloading...".to_string(),
                                DownloadStatus::Completed => "Completed".to_string(),
                                DownloadStatus::Failed(e) => format!("Failed: {}", e),
                            };
                            let style = match &d.status {
                                DownloadStatus::Queued => Style::default().fg(Color::Yellow),
                                DownloadStatus::Downloading => Style::default().fg(Color::Cyan),
                                DownloadStatus::Completed => Style::default().fg(Color::Green),
                                DownloadStatus::Failed(_) => Style::default().fg(Color::Red),
                            };
                            ListItem::new(format!("{} - {}", d.folder_name, status_str)).style(style)
                        })
                        .collect();

                    let list = List::new(items)
                        .block(Block::default().borders(Borders::ALL).title("Downloads"))
                        .highlight_style(
                            Style::default()
                                .bg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD),
                        )
                        .highlight_symbol(">> ");

                    f.render_stateful_widget(list, main_chunks[0], &mut app.downloads_list_state);
                }
                Tab::History => {
                    let items: Vec<ListItem> = app
                        .history
                        .iter()
                        .map(|h| ListItem::new(format!("{} ({})", h.folder_name, h.remote_path)))
                        .collect();

                    let list = List::new(items)
                        .block(Block::default().borders(Borders::ALL).title("History"))
                        .highlight_style(
                            Style::default()
                                .bg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD),
                        )
                        .highlight_symbol(">> ");

                    f.render_stateful_widget(list, main_chunks[0], &mut app.history_list_state);
                }
            }

            // Legend panel
            let legend_items = match app.current_tab {
                Tab::Browser => vec![
                    "j/k: Navigate",
                    "↑/↓: Navigate",
                    "PgUp/Dn: Page",
                    "Enter: Open",
                    "Bksp: Back",
                    "/: Filter",
                    "d: Download",
                    "T: Change dest",
                    "Tab: Switch tab",
                    "q: Quit",
                ],
                Tab::Downloads => vec![
                    "j/k: Navigate",
                    "↑/↓: Navigate",
                    "PgUp/Dn: Page",
                    "Tab: Switch tab",
                    "q: Quit",
                ],
                Tab::History => vec![
                    "j/k: Navigate",
                    "↑/↓: Navigate",
                    "PgUp/Dn: Page",
                    "x: Clear item",
                    "X: Clear all",
                    "Tab: Switch tab",
                    "q: Quit",
                ],
            };

            let legend_text = legend_items.join("\n");
            let legend = Paragraph::new(legend_text)
                .style(Style::default().fg(Color::Gray))
                .block(Block::default().borders(Borders::ALL).title("Keys"));
            f.render_widget(legend, main_chunks[1]);

            // Status bar / Input field
            match app.input_mode {
                InputMode::Normal => {
                    // Split status bar into left (status) and right (active download)
                    let status_chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Percentage(50),
                            Constraint::Percentage(50),
                        ])
                        .split(chunks[3]);

                    let status = Paragraph::new(app.status_message.as_str())
                        .style(Style::default().fg(Color::Yellow))
                        .block(Block::default().borders(Borders::ALL).title("Status"));
                    f.render_widget(status, status_chunks[0]);

                    // Active download section with file name and progress gauge
                    let download_info = app.active_download_info.lock().unwrap();
                    if let Some(ref progress) = *download_info {
                        // Split download section into file name (1 line) and gauge (remaining)
                        let download_chunks = Layout::default()
                            .direction(Direction::Vertical)
                            .constraints([
                                Constraint::Length(1),
                                Constraint::Min(0),
                            ])
                            .margin(1)
                            .split(status_chunks[1]);

                        // File name at top
                        let file_paragraph = Paragraph::new(progress.file_name.as_str())
                            .style(Style::default().fg(Color::Cyan));
                        f.render_widget(file_paragraph, download_chunks[0]);

                        // Progress gauge below
                        let gauge_label = format!("{}% @ {}", progress.percentage, progress.speed);
                        let gauge = Gauge::default()
                            .gauge_style(Style::default().fg(Color::Cyan).bg(Color::Black))
                            .percent(progress.percentage)
                            .label(gauge_label);
                        f.render_widget(gauge, download_chunks[1]);

                        // Render block border
                        let block = Block::default().borders(Borders::ALL).title("Active Download");
                        f.render_widget(block, status_chunks[1]);
                    } else {
                        // No active download
                        let empty = Paragraph::new("")
                            .block(Block::default().borders(Borders::ALL).title("Active Download"));
                        f.render_widget(empty, status_chunks[1]);
                    }
                }
                InputMode::EditingPath => {
                    let input = Paragraph::new(app.input_buffer.as_str())
                        .style(Style::default().fg(Color::White))
                        .block(Block::default().borders(Borders::ALL).title("Download Destination (Enter: save, Esc: cancel)"));
                    f.render_widget(input, chunks[3]);
                }
                InputMode::Filtering => {
                    let input = Paragraph::new(app.input_buffer.as_str())
                        .style(Style::default().fg(Color::White))
                        .block(Block::default().borders(Borders::ALL).title("Filter (Enter: confirm, Esc: cancel)"));
                    f.render_widget(input, chunks[3]);
                }
            }
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match app.input_mode {
                    InputMode::Normal => {
                        match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Char('T') => app.start_editing_path(),
                            KeyCode::Tab => app.next_tab(),
                            KeyCode::BackTab => app.prev_tab(),
                            KeyCode::Char('/') => app.start_filtering(),
                            KeyCode::Char('d') => app.queue_download(),
                            KeyCode::Char('x') => app.clear_history_item(),
                            KeyCode::Char('X') => app.clear_all_history(),
                            KeyCode::Enter => {
                                app.enter_folder()?;
                            }
                            KeyCode::Backspace => {
                                app.go_back()?;
                            }
                            KeyCode::Down | KeyCode::Char('j') => app.next(),
                            KeyCode::Up | KeyCode::Char('k') => app.previous(),
                            KeyCode::PageUp => app.page_up(),
                            KeyCode::PageDown => app.page_down(),
                            _ => {}
                        }
                    }
                    InputMode::EditingPath => {
                        match key.code {
                            KeyCode::Enter => app.confirm_path_change(),
                            KeyCode::Esc => app.cancel_input(),
                            KeyCode::Backspace => app.handle_input_backspace(),
                            KeyCode::Char(c) => app.handle_input_char(c),
                            _ => {}
                        }
                    }
                    InputMode::Filtering => {
                        match key.code {
                            KeyCode::Enter => app.confirm_filter(),
                            KeyCode::Esc => app.cancel_filter(),
                            KeyCode::Backspace => app.handle_input_backspace(),
                            KeyCode::Char(c) => app.handle_input_char(c),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}
