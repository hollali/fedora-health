use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use super::demo;
use super::themes::{ACCENT, MUTED};
use super::{disk_tab, doctor_tab, history_tab, launch_tab, live_tab, services_tab};
use crate::apps::{self, DesktopEntry};
use crate::doctor::{self, DiskInfo, Finding};
use crate::launch::{self, LaunchOptions, LaunchReport, Progress};
use crate::logfmt::{self, LaunchRecord, LogSample};
use crate::prewarm;
use crate::sampler::{self, CpuSampler, IoAggregator, IoRates, LiveSample};
use crate::services::{self, ServiceAction, ServiceInfo};

pub const TAB_TITLES: [&str; 6] = ["Live", "History", "Doctor", "Services", "Disk", "Launch"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Live,
    History,
    Doctor,
    Services,
    Disk,
    Launch,
}

impl Tab {
    pub fn index(self) -> usize {
        match self {
            Self::Live => 0,
            Self::History => 1,
            Self::Doctor => 2,
            Self::Services => 3,
            Self::Disk => 4,
            Self::Launch => 5,
        }
    }

    pub fn from_index(index: usize) -> Self {
        match index % 6 {
            0 => Self::Live,
            1 => Self::History,
            2 => Self::Doctor,
            3 => Self::Services,
            4 => Self::Disk,
            _ => Self::Launch,
        }
    }

    pub fn next(self) -> Self {
        Self::from_index(self.index() + 1)
    }

    pub fn previous(self) -> Self {
        Self::from_index(self.index() + 5)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortMode {
    Total,
    Read,
    Write,
}

impl SortMode {
    pub fn next(self) -> Self {
        match self {
            Self::Total => Self::Read,
            Self::Read => Self::Write,
            Self::Write => Self::Total,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Total => "total",
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

pub enum AppEvent {
    Sample(LiveSample),
    DoctorLoaded(Vec<Finding>),
    DoctorError(String),
    AppsLoaded(String),
    DiskLoaded(DiskInfo),
    ServicesLoaded(Result<Vec<ServiceInfo>, String>),
    ServiceActionDone(Result<String, String>),
    FixPreview {
        id: String,
        result: Result<String, String>,
    },
    FixLine(String),
    FixDone(Result<(), String>),
    LaunchProgress(Progress),
    LaunchDone(Result<LaunchReport, String>),
}

#[derive(Clone, Debug)]
pub enum ConfirmAction {
    Launch {
        app_index: usize,
        cold: bool,
        stage: u8,
    },
    Prewarm {
        app_index: usize,
    },
    Service {
        unit: String,
        action: ServiceAction,
    },
    FixAll,
}

#[derive(Clone, Debug)]
pub enum Popup {
    Help,
    Confirm {
        title: String,
        body: String,
        action: ConfirmAction,
    },
    Error {
        title: String,
        body: String,
    },
    FixConfirm {
        id: String,
        commands: String,
    },
    FixRun {
        id: String,
        lines: Vec<String>,
        done: Option<Result<(), String>>,
    },
}

pub struct App {
    pub tab: Tab,
    pub demo: bool,
    pub interval: u64,
    pub should_quit: bool,
    pub now: u64,
    pub live: VecDeque<LogSample>,
    pub live_rates: Vec<IoRates>,
    pub history: Vec<LogSample>,
    pub paused: bool,
    pub sort: SortMode,
    pub top3_streak: HashMap<String, u32>,
    pub doctor: Vec<Finding>,
    pub doctor_selected: usize,
    pub doctor_error: Option<String>,
    pub doctor_loading: bool,
    pub preview_loading: bool,
    pub startup_text: String,
    pub apps: Vec<DesktopEntry>,
    pub app_selected: usize,
    pub app_filter: String,
    pub filter_editing: bool,
    pub launch_history: Vec<LaunchRecord>,
    pub launch_progress: Option<Progress>,
    pub launch_running: bool,
    pub disk: DiskInfo,
    pub disk_loading: bool,
    pub services: Vec<ServiceInfo>,
    pub service_selected: usize,
    pub services_loading: bool,
    pub services_error: Option<String>,
    pub popup: Option<Popup>,
    pub status: String,
    pub history_offset: usize,
    pub history_zoom: u32,
}

impl App {
    pub fn new(demo_mode: bool, interval: u64) -> Self {
        let now = logfmt::now_epoch();
        let (history, doctor, apps_list, launch_history, disk, startup_text, services) =
            if demo_mode {
                (
                    demo::seed_history(now),
                    demo::fake_findings(),
                    demo::fake_apps(),
                    demo::fake_launch_history(now),
                    demo::fake_disk(),
                    demo::startup_text(),
                    demo::fake_services(),
                )
            } else {
                (
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    DiskInfo::default(),
                    String::new(),
                    Vec::new(),
                )
            };
        let live: VecDeque<LogSample> = history
            .iter()
            .filter(|sample| sample.ts.saturating_add(600) >= now)
            .cloned()
            .collect();
        let live_rates = demo_mode
            .then(|| demo::fake_sample(1, now))
            .map(|sample| sample.rates)
            .unwrap_or_default();
        Self {
            tab: Tab::Live,
            demo: demo_mode,
            interval,
            should_quit: false,
            now,
            live,
            live_rates,
            history,
            paused: false,
            sort: SortMode::Total,
            top3_streak: HashMap::new(),
            doctor,
            doctor_selected: 0,
            doctor_error: None,
            doctor_loading: !demo_mode,
            preview_loading: false,
            startup_text,
            apps: apps_list,
            app_selected: 0,
            app_filter: String::new(),
            filter_editing: false,
            launch_history,
            launch_progress: None,
            launch_running: false,
            disk,
            disk_loading: !demo_mode,
            services,
            service_selected: 0,
            services_loading: false,
            services_error: None,
            popup: None,
            status: if demo_mode {
                "DEMO: fake data; nothing is fixed, launched or written".to_string()
            } else {
                String::new()
            },
            history_offset: 0,
            history_zoom: 1,
        }
    }

    pub fn start(&mut self, tx: Sender<AppEvent>) {
        if self.demo {
            demo::spawn_sampler(tx, self.interval);
            return;
        }
        self.reload_history();
        self.launch_history = launch::load_history();
        self.apps = apps::installed_apps();
        self.spawn_sampler(tx.clone());
        self.refresh_doctor(tx.clone());
        self.reload_disk(tx);
    }

    fn spawn_sampler(&self, tx: Sender<AppEvent>) {
        let interval = self.interval.max(1);
        thread::spawn(move || {
            let mut cpu = CpuSampler::new();
            cpu.prime();
            let mut io = IoAggregator::default();
            io.prime(&sampler::read_all_procs());
            loop {
                let started = Instant::now();
                thread::sleep(Duration::from_secs(interval));
                let sample = sampler::sample_live(&mut cpu, &mut io, started.elapsed());
                if tx.send(AppEvent::Sample(sample)).is_err() {
                    break;
                }
            }
        });
    }

    pub fn reload_history(&mut self) {
        if self.demo {
            return;
        }
        let mut loaded = logfmt::read_samples(&logfmt::default_log_path());
        loaded.sort_by_key(|sample| sample.ts);
        self.history = loaded;
        let cutoff = self.now.saturating_sub(600);
        self.live = self
            .history
            .iter()
            .filter(|sample| sample.ts >= cutoff)
            .cloned()
            .collect();
    }

    pub fn refresh_doctor(&mut self, tx: Sender<AppEvent>) {
        if self.demo {
            self.doctor = demo::fake_findings();
            self.startup_text = demo::startup_text();
            self.doctor_loading = false;
            return;
        }
        self.doctor_loading = true;
        self.doctor_error = None;
        let doctor_tx = tx.clone();
        thread::spawn(move || match doctor::run_json(None) {
            Ok(findings) => {
                let _ = doctor_tx.send(AppEvent::DoctorLoaded(findings));
            }
            Err(err) => {
                let _ = doctor_tx.send(AppEvent::DoctorError(err));
            }
        });
        thread::spawn(move || {
            let text = apps::render_audit();
            let _ = tx.send(AppEvent::AppsLoaded(text));
        });
    }

    pub fn reload_disk(&mut self, tx: Sender<AppEvent>) {
        if self.demo {
            self.disk = demo::fake_disk();
            self.disk_loading = false;
            return;
        }
        self.disk_loading = true;
        thread::spawn(move || {
            let info = doctor::collect_disk_info();
            let _ = tx.send(AppEvent::DiskLoaded(info));
        });
    }

    pub fn refresh_services(&mut self, tx: Sender<AppEvent>) {
        if self.demo {
            self.services = demo::fake_services();
            self.services_loading = false;
            self.services_error = None;
            return;
        }
        self.services_loading = true;
        self.services_error = None;
        thread::spawn(move || {
            let result = services::collect_services();
            let _ = tx.send(AppEvent::ServicesLoaded(result));
        });
    }

    pub fn on_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Sample(sample) => {
                self.now = self.now.max(sample.log.ts);
                if !self.paused {
                    self.apply_sample(sample);
                }
            }
            AppEvent::DoctorLoaded(findings) => {
                self.doctor = findings;
                self.doctor_loading = false;
                self.doctor_error = None;
                if self.doctor_selected >= self.doctor.len() {
                    self.doctor_selected = self.doctor.len().saturating_sub(1);
                }
            }
            AppEvent::DoctorError(err) => {
                self.doctor_loading = false;
                self.doctor_error = Some(err);
            }
            AppEvent::AppsLoaded(text) => self.startup_text = text,
            AppEvent::DiskLoaded(disk) => {
                self.disk = disk;
                self.disk_loading = false;
            }
            AppEvent::ServicesLoaded(result) => {
                self.services_loading = false;
                match result {
                    Ok(list) => {
                        self.services = list;
                        self.services_error = None;
                        if self.service_selected >= self.services.len() {
                            self.service_selected = self.services.len().saturating_sub(1);
                        }
                    }
                    Err(err) => {
                        self.services_error = Some(err);
                    }
                }
            }
            AppEvent::ServiceActionDone(result) => match result {
                Ok(message) => {
                    self.status = message.lines().next().unwrap_or("done").to_string();
                }
                Err(err) => {
                    self.status = "service action failed".to_string();
                    self.popup = Some(Popup::Error {
                        title: "Service action failed".into(),
                        body: err,
                    });
                }
            },
            AppEvent::FixPreview { id, result } => {
                self.preview_loading = false;
                match result {
                    Ok(commands) => {
                        self.popup = Some(Popup::FixConfirm { id, commands });
                    }
                    Err(err) => {
                        self.popup = Some(Popup::Error {
                            title: "Cannot prepare fix".into(),
                            body: err,
                        });
                    }
                }
            }
            AppEvent::FixLine(line) => {
                if let Some(Popup::FixRun { lines, .. }) = &mut self.popup {
                    lines.push(line);
                    if lines.len() > 400 {
                        lines.remove(0);
                    }
                }
            }
            AppEvent::FixDone(result) => {
                let ok = result.is_ok();
                if let Some(Popup::FixRun { done, .. }) = &mut self.popup {
                    *done = Some(result);
                } else {
                    self.status = match result {
                        Ok(()) => "fix finished".to_string(),
                        Err(err) => format!("fix failed: {err}"),
                    };
                }
                if ok {
                    self.status = "fix finished; press r on Doctor to re-check".to_string();
                }
            }
            AppEvent::LaunchProgress(progress) => {
                self.launch_progress = Some(progress);
            }
            AppEvent::LaunchDone(result) => {
                self.launch_running = false;
                self.launch_progress = None;
                match result {
                    Ok(report) => {
                        if let Some(app) = &report.already_running {
                            self.status =
                                format!("{app} was already running; close it before benchmarking");
                        } else {
                            for result in &report.results {
                                self.launch_history.push(result.record.clone());
                            }
                            if let Some(ratio) = report.cold_warm_ratio {
                                self.status =
                                    format!("benchmark finished; cold/warm ratio {ratio:.2}x");
                            } else if let Some(result) = report.results.last() {
                                let mut message = format!(
                                    "benchmark finished: {} settled in {:.1}s, {:.0} MB read, D {:.0}%",
                                    result.record.app,
                                    result.record.settled_ms as f64 / 1000.0,
                                    result.record.read_bytes as f64 / 1_048_576.0,
                                    result.record.d_pct
                                );
                                if let Some((dir, _)) = result.trace_dirs.first() {
                                    message.push_str(&format!("; hottest dir {dir}"));
                                }
                                self.status = message;
                            } else {
                                self.status = "benchmark finished".to_string();
                            }
                        }
                    }
                    Err(err) => {
                        self.popup = Some(Popup::Error {
                            title: "Benchmark failed".into(),
                            body: err,
                        });
                    }
                }
            }
        }
    }

    fn apply_sample(&mut self, sample: LiveSample) {
        let top: HashSet<String> = sample
            .log
            .procs
            .iter()
            .map(|proc| proc.name.clone())
            .collect();
        for name in self.top3_streak.keys().cloned().collect::<Vec<_>>() {
            if !top.contains(&name) {
                self.top3_streak.remove(&name);
            }
        }
        for name in &top {
            let count = self.top3_streak.entry(name.clone()).or_insert(0);
            *count = count.saturating_add(1);
        }
        self.live.push_back(sample.log.clone());
        let cutoff = self.now.saturating_sub(600);
        while self.live.front().is_some_and(|sample| sample.ts < cutoff) {
            self.live.pop_front();
        }
        if self.demo {
            self.history.push(sample.log.clone());
            if self.history.len() > 5000 {
                self.history.remove(0);
            }
        }
        self.live_rates = sample.rates;
    }

    pub fn top_streak_name(&self) -> Option<&str> {
        self.top3_streak
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(name, _)| name.as_str())
    }

    pub fn visible_history(&self) -> &[LogSample] {
        let len = self.history.len();
        if len == 0 {
            return &[];
        }
        let zoom = self.history_zoom.max(1) as usize;
        let window = (len / zoom).max(20).min(len);
        let max_offset = len.saturating_sub(window);
        let offset = self.history_offset.min(max_offset);
        let end = len.saturating_sub(offset);
        let start = end.saturating_sub(window);
        &self.history[start..end]
    }

    pub fn filtered_app_indices(&self) -> Vec<usize> {
        let needle = self.app_filter.to_lowercase();
        self.apps
            .iter()
            .enumerate()
            .filter(|(_, app)| {
                needle.is_empty()
                    || app.name.to_lowercase().contains(&needle)
                    || app.id.to_lowercase().contains(&needle)
                    || app.exec.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn selected_app_index(&self) -> Option<usize> {
        self.filtered_app_indices().get(self.app_selected).copied()
    }

    pub fn app_records(&self, index: usize) -> Vec<&LaunchRecord> {
        let Some(app) = self.apps.get(index) else {
            return Vec::new();
        };
        let argv = apps::parse_exec_argv(&app.exec);
        let name = launch::app_name(&argv);
        let mut records: Vec<&LaunchRecord> = self
            .launch_history
            .iter()
            .filter(|record| record.app == name)
            .collect();
        records.sort_by_key(|record| record.ts);
        records
    }

    pub fn matching_findings(&self, index: usize) -> Vec<&Finding> {
        let records = self.app_records(index);
        let Some(latest) = records.last() else {
            return Vec::new();
        };
        let priority: &[&str] = match latest.verdict.as_str() {
            "waiting" => &["startup-dns", "startup-dbus-keyring", "startup-autostart"],
            "disk-bound" => &[
                "startup-fontcache",
                "startup-iconcache",
                "startup-flatpak",
                "startup-shadercache",
                "startup-ldconfig",
            ],
            _ => &["startup-autostart"],
        };
        let mut findings: Vec<&Finding> = self
            .doctor
            .iter()
            .filter(|finding| {
                priority.contains(&finding.id.as_str())
                    || (finding.id.starts_with("startup-")
                        && finding.severity != crate::report::Severity::Ok)
            })
            .collect();
        findings.sort_by_key(|finding| {
            priority
                .iter()
                .position(|id| *id == finding.id)
                .unwrap_or(priority.len())
        });
        findings
    }

    pub fn on_key(&mut self, key: KeyEvent, tx: &Sender<AppEvent>) {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            self.should_quit = true;
            return;
        }
        if matches!(self.popup, Some(Popup::Help)) {
            self.popup = None;
            return;
        }
        if self.popup.is_some() {
            self.handle_popup_key(key, tx);
            return;
        }
        if self.filter_editing {
            self.handle_filter_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return;
            }
            KeyCode::Char('?') => {
                self.popup = Some(Popup::Help);
                return;
            }
            KeyCode::Tab => {
                self.tab = self.tab.next();
                self.on_tab_change(tx);
                return;
            }
            KeyCode::BackTab => {
                self.tab = self.tab.previous();
                self.on_tab_change(tx);
                return;
            }
            KeyCode::Char('1') => {
                self.tab = Tab::Live;
                return;
            }
            KeyCode::Char('2') => {
                self.tab = Tab::History;
                self.on_tab_change(tx);
                return;
            }
            KeyCode::Char('3') => {
                self.tab = Tab::Doctor;
                self.on_tab_change(tx);
                return;
            }
            KeyCode::Char('4') => {
                self.tab = Tab::Services;
                self.on_tab_change(tx);
                return;
            }
            KeyCode::Char('5') => {
                self.tab = Tab::Disk;
                return;
            }
            KeyCode::Char('6') => {
                self.tab = Tab::Launch;
                return;
            }
            _ => {}
        }

        let down = matches!(key.code, KeyCode::Down | KeyCode::Char('j'));
        let up = matches!(key.code, KeyCode::Up | KeyCode::Char('k'));
        let left = matches!(key.code, KeyCode::Left | KeyCode::Char('h'));
        let right = matches!(key.code, KeyCode::Right | KeyCode::Char('l'));

        match self.tab {
            Tab::Live => match key.code {
                KeyCode::Char('s') => {
                    self.sort = self.sort.next();
                    self.status = format!("sort: {}", self.sort.label());
                }
                KeyCode::Char(' ') => {
                    self.paused = !self.paused;
                    self.status = if self.paused {
                        "paused".to_string()
                    } else {
                        "resumed".to_string()
                    };
                }
                _ => {}
            },
            Tab::History => {
                let len = self.history.len();
                let zoom = self.history_zoom.max(1) as usize;
                let window = (len / zoom).max(20).min(len.max(1));
                let step = (window / 10).max(1);
                if left {
                    self.history_offset =
                        (self.history_offset + step).min(len.saturating_sub(window));
                } else if right {
                    self.history_offset = self.history_offset.saturating_sub(step);
                } else if matches!(key.code, KeyCode::Char('+') | KeyCode::Char('=')) {
                    self.history_zoom = (self.history_zoom + 1).min(50);
                } else if key.code == KeyCode::Char('-') {
                    self.history_zoom = self.history_zoom.saturating_sub(1).max(1);
                }
            }
            Tab::Doctor => {
                if up {
                    self.doctor_selected = self.doctor_selected.saturating_sub(1);
                } else if down {
                    if self.doctor_selected + 1 < self.doctor.len() {
                        self.doctor_selected += 1;
                    }
                } else if key.code == KeyCode::Char('r') {
                    self.status = "re-running doctor checks and startup audit...".to_string();
                    self.refresh_doctor(tx.clone());
                } else if key.code == KeyCode::Char('f') {
                    self.request_fix(tx);
                } else if key.code == KeyCode::Char('a') {
                    self.request_fix_all(tx);
                }
            }
            Tab::Services => {
                if up {
                    self.service_selected = self.service_selected.saturating_sub(1);
                } else if down {
                    if self.service_selected + 1 < self.services.len() {
                        self.service_selected += 1;
                    }
                } else if key.code == KeyCode::Char('r') {
                    self.status = "reloading services...".to_string();
                    self.refresh_services(tx.clone());
                } else if key.code == KeyCode::Char('e') {
                    self.request_service_action(ServiceAction::Enable);
                } else if key.code == KeyCode::Char('d') {
                    self.request_service_action(ServiceAction::Disable);
                } else if key.code == KeyCode::Char('m') {
                    self.request_service_action(ServiceAction::Mask);
                } else if key.code == KeyCode::Char('u') {
                    self.request_service_action(ServiceAction::Unmask);
                }
            }
            Tab::Disk => {
                if key.code == KeyCode::Char('r') {
                    self.status = "reloading disk information...".to_string();
                    self.reload_disk(tx.clone());
                }
            }
            Tab::Launch => {
                let count = self.filtered_app_indices().len();
                if up {
                    self.app_selected = self.app_selected.saturating_sub(1);
                } else if down {
                    if self.app_selected + 1 < count {
                        self.app_selected += 1;
                    }
                } else if key.code == KeyCode::Char('/') {
                    self.filter_editing = true;
                } else if key.code == KeyCode::Enter {
                    self.request_launch(false);
                } else if key.code == KeyCode::Char('c') {
                    self.request_launch(true);
                } else if key.code == KeyCode::Char('p') {
                    self.request_prewarm();
                }
            }
        }
    }

    fn on_tab_change(&mut self, tx: &Sender<AppEvent>) {
        if self.tab == Tab::History && !self.demo {
            self.reload_history();
        }
        if self.tab == Tab::Doctor {
            self.status = "re-running doctor checks...".to_string();
            self.refresh_doctor(tx.clone());
        }
        if self.tab == Tab::Services {
            self.status = "loading system services...".to_string();
            self.refresh_services(tx.clone());
        }
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                self.filter_editing = false;
                self.app_selected = 0;
            }
            KeyCode::Backspace => {
                self.app_filter.pop();
                self.app_selected = 0;
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.app_filter.push(ch);
                self.app_selected = 0;
            }
            KeyCode::Down => {
                let count = self.filtered_app_indices().len();
                if self.app_selected + 1 < count {
                    self.app_selected += 1;
                }
            }
            KeyCode::Up => {
                self.app_selected = self.app_selected.saturating_sub(1);
            }
            _ => {}
        }
    }

    fn handle_popup_key(&mut self, key: KeyEvent, tx: &Sender<AppEvent>) {
        let popup = self.popup.clone();
        let Some(popup) = popup else { return };
        match popup {
            Popup::Help => self.popup = None,
            Popup::Error { .. } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                    self.popup = None;
                }
            }
            Popup::FixConfirm { id, .. } => {
                if matches!(key.code, KeyCode::Char('y')) {
                    self.start_fix(id, tx);
                } else if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q')
                ) {
                    self.popup = None;
                }
            }
            Popup::FixRun { done, .. } => {
                if done.is_some()
                    && matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q'))
                {
                    self.popup = None;
                    if !self.demo {
                        self.status = "re-running doctor checks...".to_string();
                        self.refresh_doctor(tx.clone());
                    }
                }
            }
            Popup::Confirm { action, .. } => {
                if matches!(key.code, KeyCode::Char('y')) {
                    match action {
                        ConfirmAction::Launch {
                            app_index,
                            cold,
                            stage,
                        } => {
                            if cold && stage == 1 {
                                let body = format!(
                                    "SECOND CONFIRMATION\n\nA cold run drops the page cache for the WHOLE machine.\nEverything will feel slow for a moment.\n\nProceed with the cold benchmark of {}?",
                                    self.apps
                                        .get(app_index)
                                        .map(|app| app.name.as_str())
                                        .unwrap_or("this app")
                                );
                                self.popup = Some(Popup::Confirm {
                                    title: "Cold run: really continue?".into(),
                                    body,
                                    action: ConfirmAction::Launch {
                                        app_index,
                                        cold: true,
                                        stage: 2,
                                    },
                                });
                            } else {
                                self.start_launch(app_index, cold, tx);
                            }
                        }
                        ConfirmAction::Prewarm { app_index } => {
                            self.add_prewarm(app_index);
                        }
                        ConfirmAction::Service { unit, action } => {
                            self.start_service_action(unit, action, tx);
                        }
                        ConfirmAction::FixAll => {
                            self.start_fix_all(tx);
                        }
                    }
                } else if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q')
                ) {
                    self.popup = None;
                }
            }
        }
    }

    fn request_launch(&mut self, cold: bool) {
        if self.launch_running {
            self.status = "a benchmark is already running".to_string();
            return;
        }
        let Some(index) = self.selected_app_index() else {
            self.status = "no app selected".to_string();
            return;
        };
        let Some(app) = self.apps.get(index) else {
            return;
        };
        if cold && !self.demo && !logfmt::is_root() {
            self.popup = Some(Popup::Error {
                title: "Root required".into(),
                body: "A cold run drops the page cache for the whole machine and requires root.\nRun hdd-watch with sudo.".into(),
            });
            return;
        }
        let argv = apps::parse_exec_argv(&app.exec);
        let command = argv.join(" ");
        let body = if cold {
            format!(
                "Command: hdd-watch launch --cold -- {command}\n\nThis starts the app and closes it after it settles.\nA cold run also drops the page cache (root), making the whole machine slow for a moment."
            )
        } else {
            format!(
                "Command: hdd-watch launch -- {command}\n\nThis starts the app and closes it after it settles.\nDo not benchmark an app that is already open."
            )
        };
        self.popup = Some(Popup::Confirm {
            title: if cold {
                "Benchmark cold start?".into()
            } else {
                "Benchmark app start?".into()
            },
            body,
            action: ConfirmAction::Launch {
                app_index: index,
                cold,
                stage: 1,
            },
        });
    }

    fn start_launch(&mut self, index: usize, cold: bool, tx: &Sender<AppEvent>) {
        let Some(app) = self.apps.get(index).cloned() else {
            return;
        };
        let argv = apps::parse_exec_argv(&app.exec);
        if argv.is_empty() {
            self.popup = Some(Popup::Error {
                title: "No command".into(),
                body: format!("{} has no usable Exec line.", app.name),
            });
            return;
        }
        let name = launch::app_name(&argv);
        self.popup = None;
        self.launch_running = true;
        self.launch_progress = None;
        self.status = format!(
            "benchmarking {} ({})...",
            app.name,
            if cold { "cold" } else { "warm" }
        );
        if self.demo {
            demo::spawn_launch(name, tx.clone(), cold);
            return;
        }
        let tx = tx.clone();
        thread::spawn(move || {
            let options = LaunchOptions {
                cold,
                command: argv,
                timeout_secs: 60,
                ..LaunchOptions::default()
            };
            let mut progress = |progress: Progress| {
                let _ = tx.send(AppEvent::LaunchProgress(progress));
            };
            let result = launch::run(&options, &mut progress);
            let _ = tx.send(AppEvent::LaunchDone(result));
        });
    }

    fn request_prewarm(&mut self) {
        let Some(index) = self.selected_app_index() else {
            self.status = "no app selected".to_string();
            return;
        };
        let Some(app) = self.apps.get(index) else {
            return;
        };
        let line = app.id.clone();
        self.popup = Some(Popup::Confirm {
            title: "Add to prewarm.conf?".into(),
            body: format!(
                "This exact line will be appended to /etc/hdd-watch/prewarm.conf:\n\n{line}\n\nIt will be warmed after login when the prewarm service is enabled."
            ),
            action: ConfirmAction::Prewarm { app_index: index },
        });
    }

    fn add_prewarm(&mut self, index: usize) {
        let Some(app) = self.apps.get(index) else {
            return;
        };
        self.popup = None;
        if self.demo {
            self.status = format!("DEMO: would add '{}' to prewarm.conf", app.id);
            return;
        }
        match prewarm::add_to_config(&app.id) {
            Ok(message) => self.status = message,
            Err(err) => {
                self.popup = Some(Popup::Error {
                    title: "Cannot edit prewarm.conf".into(),
                    body: format!("{err}\n\nRun the TUI with sudo, or edit /etc/hdd-watch/prewarm.conf yourself."),
                });
            }
        }
    }

    fn request_service_action(&mut self, action: ServiceAction) {
        let Some(service) = self.services.get(self.service_selected) else {
            self.status = "no service selected".to_string();
            return;
        };
        if !self.demo && !logfmt::is_root() {
            self.popup = Some(Popup::Error {
                title: "Root required".into(),
                body: "Enabling, disabling, masking and unmasking services requires root.\nRun hdd-watch with sudo.".into(),
            });
            return;
        }
        let unit = service.unit.clone();
        let state = service.state.clone();
        let active = if service.active.is_empty() {
            "unknown".to_string()
        } else {
            service.active.clone()
        };
        let mut body = format!(
            "This runs:\n\nsudo systemctl {} {}\n\nThe service will be {}.\n\nCurrent unit-file state: {state}\nRuntime state: {active}",
            action.verb(),
            unit,
            action.effect()
        );
        if matches!(action, ServiceAction::Enable | ServiceAction::Disable) && state == "static" {
            body.push_str(
                "\n\nThis unit is static, so systemctl may refuse to enable or disable it. Use mask/unmask instead if needed.",
            );
        }
        body.push_str("\n\nPress y to run, n or Esc to cancel.");
        self.popup = Some(Popup::Confirm {
            title: format!("{} {}?", action.verb(), unit),
            body,
            action: ConfirmAction::Service { unit, action },
        });
    }

    fn start_service_action(&mut self, unit: String, action: ServiceAction, tx: &Sender<AppEvent>) {
        if !self.demo && !logfmt::is_root() {
            self.popup = Some(Popup::Error {
                title: "Root required".into(),
                body: "Service changes require root.".into(),
            });
            return;
        }
        self.popup = None;
        if self.demo {
            let state = match action {
                ServiceAction::Enable => "enabled",
                ServiceAction::Disable => "disabled",
                ServiceAction::Mask => "masked",
                ServiceAction::Unmask => "disabled",
            };
            if let Some(service) = self.services.iter_mut().find(|item| item.unit == unit) {
                service.state = state.to_string();
            }
            self.status = format!("DEMO: would run systemctl {} {}", action.verb(), unit);
            return;
        }
        self.status = format!("running systemctl {} {}...", action.verb(), unit);
        let tx = tx.clone();
        thread::spawn(move || {
            let result = services::apply_action(&unit, action);
            let _ = tx.send(AppEvent::ServiceActionDone(result));
            let refresh = services::collect_services();
            let _ = tx.send(AppEvent::ServicesLoaded(refresh));
        });
    }

    fn request_fix(&mut self, tx: &Sender<AppEvent>) {
        let Some(finding) = self.doctor.get(self.doctor_selected) else {
            return;
        };
        if !finding.fix_available {
            self.status = format!("{} has no safe automatic fix", finding.id);
            return;
        }
        let id = finding.id.clone();
        if self.demo {
            self.popup = Some(Popup::FixConfirm {
                id: id.clone(),
                commands: format!("demo: sudo fedora-doctor.sh --fix-id {id}"),
            });
            return;
        }
        if !logfmt::is_root() {
            self.popup = Some(Popup::Error {
                title: "Root required".into(),
                body: "Fixes require root. Run hdd-watch with sudo to apply them.".into(),
            });
            return;
        }
        if self.preview_loading {
            return;
        }
        self.preview_loading = true;
        self.status = format!("loading preview for {id}...");
        let tx = tx.clone();
        thread::spawn(move || {
            let result = doctor::run_fix_preview(&id);
            let _ = tx.send(AppEvent::FixPreview { id, result });
        });
    }

    fn request_fix_all(&mut self, _tx: &Sender<AppEvent>) {
        if self.doctor.is_empty() {
            self.status = "no findings to fix".to_string();
            return;
        }
        let fixable: Vec<&str> = self
            .doctor
            .iter()
            .filter(|finding| finding.fix_available)
            .map(|finding| finding.id.as_str())
            .collect();
        if fixable.is_empty() {
            self.status = "no findings have a safe automatic fix".to_string();
            return;
        }
        if self.demo {
            self.popup = Some(Popup::Confirm {
                title: "Fix all safe fixes?".into(),
                body: format!("DEMO: would run all safe fixes: {}", fixable.join(", ")),
                action: ConfirmAction::FixAll,
            });
            return;
        }
        if !logfmt::is_root() {
            self.popup = Some(Popup::Error {
                title: "Root required".into(),
                body: "Fixes require root. Run hdd-watch with sudo to apply them.".into(),
            });
            return;
        }
        self.popup = Some(Popup::Confirm {
            title: "Fix all safe fixes?".into(),
            body: format!(
                "This runs every safe, idempotent fix that applies to the current findings:\n\n{}\n\nPreview first with: sudo fedora-doctor.sh --dry-run",
                fixable.join("\n")
            ),
            action: ConfirmAction::FixAll,
        });
    }

    fn start_fix_all(&mut self, tx: &Sender<AppEvent>) {
        self.popup = Some(Popup::FixRun {
            id: "all".to_string(),
            lines: vec!["running sudo fedora-doctor.sh --fix".to_string()],
            done: None,
        });
        if self.demo {
            self.status = "DEMO: would run all safe fixes".to_string();
            return;
        }
        let (fix_tx, fix_rx) = mpsc::channel();
        thread::spawn(move || doctor::run_fix_all_streaming(fix_tx));
        let tx = tx.clone();
        thread::spawn(move || {
            while let Ok(event) = fix_rx.recv() {
                let event = match event {
                    doctor::FixEvent::Line(line) => AppEvent::FixLine(line),
                    doctor::FixEvent::Done(result) => AppEvent::FixDone(result),
                };
                if tx.send(event).is_err() {
                    break;
                }
            }
        });
    }

    fn start_fix(&mut self, id: String, tx: &Sender<AppEvent>) {
        if !self.demo && !logfmt::is_root() {
            self.popup = Some(Popup::Error {
                title: "Root required".into(),
                body: "Fixes require root.".into(),
            });
            return;
        }
        self.popup = Some(Popup::FixRun {
            id: id.clone(),
            lines: vec![format!("running sudo fedora-doctor.sh --fix-id {id}")],
            done: None,
        });
        if self.demo {
            demo::spawn_fix(id, tx.clone());
            return;
        }
        let (fix_tx, fix_rx) = mpsc::channel();
        thread::spawn(move || doctor::run_fix_streaming(&id, fix_tx));
        let tx = tx.clone();
        thread::spawn(move || {
            while let Ok(event) = fix_rx.recv() {
                let event = match event {
                    doctor::FixEvent::Line(line) => AppEvent::FixLine(line),
                    doctor::FixEvent::Done(result) => AppEvent::FixDone(result),
                };
                if tx.send(event).is_err() {
                    break;
                }
            }
        });
    }

    pub fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 80 || area.height < 20 {
            let message = Paragraph::new(vec![
                Line::from("Terminal too small"),
                Line::from("Resize to at least 80 x 20, or press q to quit."),
            ])
            .alignment(Alignment::Center);
            frame.render_widget(message, area);
            return;
        }
        let banner_height = u16::from(!logfmt::is_root());
        let chunks = Layout::vertical([
            Constraint::Length(3 + banner_height),
            Constraint::Min(4),
            Constraint::Length(4),
        ])
        .split(area);
        let header = Layout::vertical([Constraint::Length(3), Constraint::Length(banner_height)])
            .split(chunks[0]);
        let titles: Vec<Line> = TAB_TITLES
            .iter()
            .enumerate()
            .map(|(index, title)| Line::from(format!("{} {title}", index + 1)))
            .collect();
        let title = if self.demo {
            "hdd-watch — DEMO"
        } else {
            "hdd-watch — HDD slowdown dashboard"
        };
        let tabs = Tabs::new(titles)
            .select(self.tab.index())
            .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            .divider("|")
            .block(Block::bordered().title(title));
        frame.render_widget(tabs, header[0]);
        if banner_height == 1 {
            let banner = Paragraph::new(
                "Not root: per-process I/O of other users is hidden; fixes and cold runs are disabled. Run with sudo for full data.",
            )
            .style(Style::default().fg(ratatui::style::Color::Yellow));
            frame.render_widget(banner, header[1]);
        }

        match self.tab {
            Tab::Live => live_tab::render(frame, chunks[1], self),
            Tab::History => history_tab::render(frame, chunks[1], self),
            Tab::Doctor => doctor_tab::render(frame, chunks[1], self),
            Tab::Services => services_tab::render(frame, chunks[1], self),
            Tab::Disk => disk_tab::render(frame, chunks[1], self),
            Tab::Launch => launch_tab::render(frame, chunks[1], self),
        }

        let hint = "Tab/1-6 switch  arrows/hjkl move  ? help  q quit";
        let footer = Paragraph::new(vec![
            Line::from(Span::styled(
                if self.status.is_empty() {
                    hint.to_string()
                } else {
                    self.status.clone()
                },
                Style::default().fg(ACCENT),
            )),
            Line::from(Span::styled(hint, Style::default().fg(MUTED))),
        ])
        .block(Block::bordered());
        frame.render_widget(footer, chunks[2]);

        if let Some(popup) = &self.popup {
            self.draw_popup(frame, popup);
        }
    }

    fn draw_popup(&self, frame: &mut Frame, popup: &Popup) {
        let (title, body) = match popup {
            Popup::Help => ("Help", help_text()),
            Popup::Confirm { title, body, .. } => (title.as_str(), body.clone()),
            Popup::Error { title, body } => (title.as_str(), body.clone()),
            Popup::FixConfirm { id, commands } => (
                "Confirm fix",
                format!(
                    "Finding: {id}\n\nThe exact command(s) that will run:\n\n{commands}\n\nPress y to run, n or Esc to cancel."
                ),
            ),
            Popup::FixRun { id, lines, done } => {
                let mut body = format!("Running fix {id}\n\n");
                let start = lines.len().saturating_sub(14);
                body.push_str(&lines[start..].join("\n"));
                body.push_str("\n\n");
                match done {
                    None => body.push_str("(running... Esc hides this window; the fix keeps running)"),
                    Some(Ok(())) => body.push_str("Finished successfully. Press Enter or Esc to close."),
                    Some(Err(err)) => body.push_str(&format!("FAILED: {err}\nPress Enter or Esc to close.")),
                }
                ("Fix output", body)
            }
        };
        let height = if matches!(popup, Popup::Help) { 80 } else { 60 };
        let area = centered_rect(82, height, frame.area());
        frame.render_widget(Clear, area);
        let paragraph = Paragraph::new(body)
            .block(Block::bordered().title(format!(" {title} ")))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }

    pub fn doctor_detail(&self) -> String {
        if let Some(error) = &self.doctor_error {
            return format!("Doctor failed:\n\n{error}");
        }
        if self.doctor_loading {
            return "Running fedora-doctor.sh --json ...".to_string();
        }
        let Some(finding) = self.doctor.get(self.doctor_selected) else {
            return "No findings.".to_string();
        };
        format!(
            "[{}] {}\nid: {}\nfix available: {}\n\n{}\n\n{}",
            finding.severity.label(),
            finding.title,
            finding.id,
            if finding.fix_available { "yes" } else { "no" },
            finding.detail,
            if finding.fix_available {
                "Press f to preview the exact command and confirm before running it."
            } else {
                "This finding is advice only; nothing will be changed automatically."
            }
        )
    }

    pub fn service_detail(&self) -> String {
        if let Some(error) = &self.services_error {
            return format!("Cannot read system services:\n\n{error}");
        }
        if self.services_loading && self.services.is_empty() {
            return "Loading systemd services...".to_string();
        }
        let Some(service) = self.services.get(self.service_selected) else {
            return "No services found.".to_string();
        };
        format!(
            "{}\n\nboot time: {}\nunit-file state: {}\nruntime state: {}\n\n\
             e enable at next boot\nd disable at next boot\nm mask\nu unmask\nr reload\n\n\
             Changes run sudo systemctl and take effect at the next boot. Running services are not stopped.\n\
             Use mask for static units that cannot be enabled or disabled.",
            service.unit,
            if service.boot_time.is_empty() {
                "-"
            } else {
                &service.boot_time
            },
            service.state,
            if service.active.is_empty() {
                "unknown"
            } else {
                &service.active
            }
        )
    }
}

pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

fn help_text() -> String {
    [
        "hdd-watch TUI — key bindings",
        "",
        "Global",
        "  Tab / Shift+Tab      next / previous tab",
        "  1..6                 jump to Live, History, Doctor, Services, Disk, Launch",
        "  q, Ctrl+C            quit",
        "  ?                    toggle this help",
        "  arrows or hjkl       move / scroll",
        "",
        "Live",
        "  s                    cycle process sort (total/read/write)",
        "  space                pause / resume live updates",
        "",
        "History",
        "  left/right or h/l    scroll older/newer",
        "  + / -                zoom in / out",
        "",
        "Doctor",
        "  up/down or k/j       select finding",
        "  f                    preview and confirm a fix",
        "  a                    fix all safe fixes (confirmation)",
        "  r                    re-run checks and startup audit",
        "",
        "Services",
        "  up/down or k/j       select system service",
        "  e                    enable at next boot (confirmation)",
        "  d                    disable at next boot (confirmation)",
        "  m                    mask (prevent starting; confirmation)",
        "  u                    unmask (confirmation)",
        "  r                    reload the service list",
        "",
        "Disk",
        "  r                    reload SMART/filesystem data",
        "",
        "Launch",
        "  up/down or k/j       select app",
        "  /                    filter apps (Esc/Enter closes filter)",
        "  enter                benchmark warm start (confirmation)",
        "  c                    benchmark cold start (two confirmations)",
        "  p                    add app to prewarm.conf (confirmation)",
        "",
        "Fixes never run without the confirmation popup. Cold runs need root.",
        "Press any key to close this help.",
    ]
    .join("\n")
}
