use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use sysinfo::{Components, ProcessesToUpdate, System};

use crate::collectors::{
    detect_cpu_model, detect_cpu_pairs, pair_entries_count, parse_logical_cpu_id, pick_cpu_temp,
    read_gpu_stats, read_root_storage, send_signal_to_pid, top_processes,
};
use crate::config::{
    PROCESS_FREEZE_AFTER_NAV, PROCESS_GROUP_DISAPPEAR_GRACE, STATUS_MESSAGE_DURATION,
};
use crate::models::{GpuStats, KillSignal, ProcessGroup, ProcessStat, StatusMessage, StorageStats};

#[derive(Debug, Clone, Copy)]
pub enum ProcessListRow {
    App {
        app_index: usize,
    },
    Process {
        app_index: usize,
        process_index: usize,
    },
}

#[derive(Debug, Clone)]
pub enum KillMenuTarget {
    Process {
        pid: String,
        name: String,
    },
    App {
        app: String,
        pids: Vec<String>,
        process_count: usize,
    },
}

#[derive(Debug, Clone)]
struct CachedProcessGroup {
    group: ProcessGroup,
    last_seen: Instant,
}

#[derive(Debug)]
pub struct App {
    pub started_at: Instant,
    pub system: System,
    pub components: Components,
    pub cpu_usage_by_id: BTreeMap<usize, f32>,
    pub cpu_pairs: Vec<(usize, Option<usize>)>,
    pub cpu_model: String,
    pub cpu_temp_c: Option<f32>,
    pub cpu_freq_mhz: Option<u64>,
    pub storage: Option<StorageStats>,
    pub process_groups: Vec<ProcessGroup>,
    process_group_history: BTreeMap<String, CachedProcessGroup>,
    pub gpu: Option<GpuStats>,
    pub selected_row: usize,
    pub expanded_app: Option<String>,
    pub process_freeze_until: Option<Instant>,
    pub kill_menu_open: bool,
    pub kill_menu_target: Option<KillMenuTarget>,
    pub status_message: Option<StatusMessage>,
    pub combine_smt: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            system: System::new_all(),
            components: Components::new_with_refreshed_list(),
            cpu_usage_by_id: BTreeMap::new(),
            cpu_pairs: Vec::new(),
            cpu_model: String::new(),
            cpu_temp_c: None,
            cpu_freq_mhz: None,
            storage: None,
            process_groups: Vec::new(),
            process_group_history: BTreeMap::new(),
            gpu: None,
            selected_row: 0,
            expanded_app: None,
            process_freeze_until: None,
            kill_menu_open: false,
            kill_menu_target: None,
            status_message: None,
            combine_smt: true,
        }
    }

    pub fn refresh_cpu(&mut self) {
        self.system.refresh_cpu_all();
        self.components.refresh(false);
        if self.cpu_model.is_empty() {
            self.cpu_model = detect_cpu_model(&self.system);
        }
        self.cpu_freq_mhz = self.system.cpus().first().map(|c| c.frequency());
        self.cpu_usage_by_id.clear();
        for (idx, cpu) in self.system.cpus().iter().enumerate() {
            let id = parse_logical_cpu_id(cpu.name(), idx);
            self.cpu_usage_by_id.insert(id, cpu.cpu_usage());
        }
        if pair_entries_count(&self.cpu_pairs) != self.cpu_usage_by_id.len() {
            let ids = self.cpu_usage_by_id.keys().copied().collect::<Vec<_>>();
            self.cpu_pairs = detect_cpu_pairs(ids);
        }
        self.cpu_temp_c = pick_cpu_temp(&self.components);
    }

    pub fn refresh_processes(&mut self) {
        self.system.refresh_memory();
        self.system.refresh_processes(
            ProcessesToUpdate::All,
            /* remove_dead_processes */ true,
        );

        let now = Instant::now();
        let processes = top_processes(&self.system, usize::MAX);
        let fresh_groups = group_processes_by_app(processes);
        self.process_groups = stabilize_process_groups(
            &mut self.process_group_history,
            fresh_groups,
            now,
            PROCESS_GROUP_DISAPPEAR_GRACE,
        );

        if let Some(expanded_app) = &self.expanded_app {
            let still_exists = self
                .process_groups
                .iter()
                .any(|group| group.app == *expanded_app);
            if !still_exists {
                self.expanded_app = None;
            }
        }

        self.sync_selection();
    }

    pub fn refresh_gpu(&mut self) {
        self.gpu = read_gpu_stats();
    }

    pub fn refresh_storage(&mut self) {
        self.storage = read_root_storage();
    }

    pub fn refresh_all(&mut self) {
        self.refresh_cpu();
        self.refresh_processes();
        self.refresh_gpu();
        self.refresh_storage();
    }

    pub fn visible_process_rows(&self) -> Vec<ProcessListRow> {
        let mut rows = Vec::new();
        for (app_index, group) in self.process_groups.iter().enumerate() {
            rows.push(ProcessListRow::App { app_index });
            if self
                .expanded_app
                .as_deref()
                .is_some_and(|expanded| expanded == group.app)
            {
                for process_index in 0..group.processes.len() {
                    rows.push(ProcessListRow::Process {
                        app_index,
                        process_index,
                    });
                }
            }
        }
        rows
    }

    pub fn selected_row_index(&self) -> Option<usize> {
        let row_count = self.visible_process_rows().len();
        if row_count == 0 {
            None
        } else {
            Some(self.selected_row.min(row_count - 1))
        }
    }

    pub fn selected_row_kind(&self) -> Option<ProcessListRow> {
        let rows = self.visible_process_rows();
        rows.get(self.selected_row).copied()
    }

    pub fn process_updates_frozen(&self, now: Instant) -> bool {
        self.process_freeze_until.is_some_and(|until| now < until)
    }

    pub fn process_updates_locked(&self, now: Instant) -> bool {
        self.kill_menu_open || self.process_updates_frozen(now) || self.expanded_app.is_some()
    }

    pub fn process_freeze_remaining(&self, now: Instant) -> Option<Duration> {
        self.process_freeze_until
            .and_then(|until| (until > now).then(|| until.duration_since(now)))
    }

    pub fn expanded_app_name(&self) -> Option<&str> {
        self.expanded_app.as_deref()
    }

    pub fn is_expanded_app_index(&self, app_index: usize) -> bool {
        let Some(group) = self.process_groups.get(app_index) else {
            return false;
        };
        self.expanded_app
            .as_deref()
            .is_some_and(|expanded| expanded == group.app)
    }

    pub fn process_group_at(&self, app_index: usize) -> Option<&ProcessGroup> {
        self.process_groups.get(app_index)
    }

    pub fn process_at(&self, app_index: usize, process_index: usize) -> Option<&ProcessStat> {
        self.process_groups
            .get(app_index)
            .and_then(|group| group.processes.get(process_index))
    }

    pub fn kill_menu_target(&self) -> Option<&KillMenuTarget> {
        self.kill_menu_target.as_ref()
    }

    pub fn move_selection(&mut self, delta: isize, now: Instant) {
        let row_count = self.visible_process_rows().len();
        if row_count == 0 {
            return;
        }

        let max_idx = row_count.saturating_sub(1);
        let next = if delta.is_negative() {
            self.selected_row.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected_row
                .saturating_add(delta as usize)
                .min(max_idx)
        };

        self.selected_row = next;
        self.freeze_process_list(now);
    }

    pub fn expand_selected_app(&mut self) -> bool {
        let Some(ProcessListRow::App { app_index }) = self.selected_row_kind() else {
            return false;
        };
        let Some(group) = self.process_groups.get(app_index) else {
            return false;
        };
        if group.stale {
            return false;
        }

        self.expanded_app = Some(group.app.clone());
        if let Some(first_child_row) = self.row_index_for_child(app_index, 0) {
            self.selected_row = first_child_row;
        }
        self.sync_selection();
        true
    }

    pub fn collapse_expanded_app(&mut self) -> bool {
        let Some(expanded_app) = self.expanded_app.take() else {
            return false;
        };

        let app_index = self
            .process_groups
            .iter()
            .position(|group| group.app == expanded_app);

        if let Some(app_index) = app_index {
            self.selected_row = app_index;
        }
        self.sync_selection();
        true
    }

    pub fn open_kill_menu(&mut self, now: Instant) {
        match self.selected_row_kind() {
            Some(ProcessListRow::Process {
                app_index,
                process_index,
            }) => {
                let Some(process) = self.process_at(app_index, process_index) else {
                    self.set_status(now, "No process selected", true);
                    return;
                };
                self.kill_menu_target = Some(KillMenuTarget::Process {
                    pid: process.pid.clone(),
                    name: process.name.clone(),
                });
                self.kill_menu_open = true;
                self.freeze_process_list(now);
            }
            Some(ProcessListRow::App { app_index }) => {
                let Some(group) = self.process_group_at(app_index) else {
                    self.set_status(now, "No app selected", true);
                    return;
                };
                if group.stale {
                    self.set_status(now, "App is no longer active", true);
                    return;
                }
                let pids = group
                    .processes
                    .iter()
                    .map(|process| process.pid.clone())
                    .collect::<Vec<_>>();
                self.kill_menu_target = Some(KillMenuTarget::App {
                    app: group.app.clone(),
                    process_count: group.processes.len(),
                    pids,
                });
                self.kill_menu_open = true;
                self.freeze_process_list(now);
            }
            None => {
                self.set_status(now, "Nothing selected", true);
            }
        }
    }

    pub fn close_kill_menu(&mut self) {
        self.kill_menu_open = false;
        self.kill_menu_target = None;
    }

    pub fn expand_kill_menu_app_target(&mut self) -> bool {
        let Some(KillMenuTarget::App { app, .. }) = self.kill_menu_target.as_ref() else {
            return false;
        };
        let app = app.clone();

        let Some(app_index) = self
            .process_groups
            .iter()
            .position(|group| !group.stale && group.app == app)
        else {
            return false;
        };

        self.expanded_app = Some(app);
        if let Some(first_child_row) = self.row_index_for_child(app_index, 0) {
            self.selected_row = first_child_row;
        }
        self.sync_selection();
        true
    }

    pub fn set_status(&mut self, now: Instant, message: impl Into<String>, is_error: bool) {
        self.status_message = Some(StatusMessage {
            text: message.into(),
            is_error,
            expires_at: now + STATUS_MESSAGE_DURATION,
        });
    }

    pub fn clear_expired_state(&mut self, now: Instant) {
        if self.process_freeze_until.is_some_and(|until| now >= until) {
            self.process_freeze_until = None;
        }
        if self
            .status_message
            .as_ref()
            .is_some_and(|status| now >= status.expires_at)
        {
            self.status_message = None;
        }
    }

    pub fn send_kill_to_selected(&self, signal: KillSignal) -> Result<String> {
        let target = self
            .kill_menu_target
            .as_ref()
            .ok_or_else(|| anyhow!("No action target selected"))?;

        match target {
            KillMenuTarget::Process { pid, name } => {
                send_signal_to_pid(pid, signal)?;
                Ok(format!("{} sent to PID {pid} ({name})", signal.label()))
            }
            KillMenuTarget::App {
                app,
                pids,
                process_count,
            } => {
                if pids.is_empty() {
                    return Err(anyhow!("App has no processes to signal"));
                }

                let mut ok = 0usize;
                let mut first_error = None;
                for pid in pids {
                    match send_signal_to_pid(pid, signal) {
                        Ok(()) => ok += 1,
                        Err(err) => {
                            if first_error.is_none() {
                                first_error = Some(format!("PID {pid}: {err}"));
                            }
                        }
                    }
                }

                if ok == pids.len() {
                    Ok(format!(
                        "{} sent to app {app} ({ok}/{process_count} processes)",
                        signal.label()
                    ))
                } else if ok > 0 {
                    let detail = first_error.unwrap_or_else(|| "unknown error".to_string());
                    Err(anyhow!(
                        "{} partially sent to app {app} ({ok}/{process_count}). {}",
                        signal.label(),
                        detail
                    ))
                } else {
                    let detail = first_error.unwrap_or_else(|| "unknown error".to_string());
                    Err(anyhow!(
                        "failed to send {} to app {app}: {}",
                        signal.label(),
                        detail
                    ))
                }
            }
        }
    }

    fn row_index_for_child(&self, app_index: usize, process_index: usize) -> Option<usize> {
        let mut row_idx = 0usize;

        for (idx, group) in self.process_groups.iter().enumerate() {
            if idx == app_index {
                if self
                    .expanded_app
                    .as_deref()
                    .is_some_and(|expanded| expanded == group.app)
                    && process_index < group.processes.len()
                {
                    return Some(row_idx + 1 + process_index);
                }
                return None;
            }

            row_idx += 1;
            if self
                .expanded_app
                .as_deref()
                .is_some_and(|expanded| expanded == group.app)
            {
                row_idx += group.processes.len();
            }
        }

        None
    }

    fn sync_selection(&mut self) {
        let row_count = self.visible_process_rows().len();
        if row_count == 0 {
            self.selected_row = 0;
            self.close_kill_menu();
            return;
        }

        self.selected_row = self.selected_row.min(row_count - 1);
        if self.kill_menu_open && self.kill_menu_target.is_none() {
            self.kill_menu_open = false;
        }
    }

    fn freeze_process_list(&mut self, now: Instant) {
        self.process_freeze_until = Some(now + PROCESS_FREEZE_AFTER_NAV);
    }
}

fn group_processes_by_app(processes: Vec<ProcessStat>) -> Vec<ProcessGroup> {
    let mut by_app: BTreeMap<String, Vec<ProcessStat>> = BTreeMap::new();
    for process in processes {
        by_app.entry(process.app.clone()).or_default().push(process);
    }

    let mut groups: Vec<ProcessGroup> = by_app
        .into_iter()
        .map(|(app, mut processes)| {
            processes.sort_by(|a, b| {
                b.cpu
                    .partial_cmp(&a.cpu)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| b.mem_mib.cmp(&a.mem_mib))
            });

            let total_mem_mib = processes.iter().map(|process| process.mem_mib).sum::<u64>();
            let total_cpu = processes.iter().map(|process| process.cpu).sum::<f32>();

            ProcessGroup {
                app,
                total_mem_mib,
                total_cpu,
                stale: false,
                processes,
            }
        })
        .collect();

    groups.sort_by(|a, b| {
        b.total_cpu
            .partial_cmp(&a.total_cpu)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.total_mem_mib.cmp(&a.total_mem_mib))
            .then_with(|| a.app.cmp(&b.app))
    });

    groups
}

fn stabilize_process_groups(
    history: &mut BTreeMap<String, CachedProcessGroup>,
    fresh_groups: Vec<ProcessGroup>,
    now: Instant,
    grace: Duration,
) -> Vec<ProcessGroup> {
    let mut fresh_by_app: BTreeMap<String, ProcessGroup> = BTreeMap::new();
    for group in fresh_groups {
        history.insert(
            group.app.clone(),
            CachedProcessGroup {
                group: group.clone(),
                last_seen: now,
            },
        );
        fresh_by_app.insert(group.app.clone(), group);
    }

    let mut combined = Vec::new();
    for fresh in fresh_by_app.values() {
        combined.push(fresh.clone());
    }

    for (app, cached) in history.iter() {
        if fresh_by_app.contains_key(app) {
            continue;
        }
        if now.duration_since(cached.last_seen) > grace {
            continue;
        }

        let mut retained = cached.group.clone();
        retained.stale = true;
        combined.push(retained);
    }

    history.retain(|_, cached| now.duration_since(cached.last_seen) <= grace);

    combined.sort_by(|a, b| {
        a.stale
            .cmp(&b.stale)
            .then_with(|| {
                b.total_cpu
                    .partial_cmp(&a.total_cpu)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| b.total_mem_mib.cmp(&a.total_mem_mib))
            .then_with(|| a.app.cmp(&b.app))
    });

    combined
}
