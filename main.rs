use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Stdout};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::{Color, Line, Modifier, Span, Style};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table};
use ratatui::{Frame, Terminal};
use sysinfo::{Components, ProcessesToUpdate, System};

const DRAW_INTERVAL: Duration = Duration::from_millis(240);
const CPU_INTERVAL: Duration = Duration::from_millis(800);
const PROCESS_INTERVAL: Duration = Duration::from_millis(800);
const GPU_INTERVAL: Duration = Duration::from_millis(800);
const STORAGE_INTERVAL: Duration = Duration::from_secs(4);
const PROCESS_FREEZE_AFTER_NAV: Duration = Duration::from_secs(2);
const STATUS_MESSAGE_DURATION: Duration = Duration::from_secs(3);

const COLOR_BG: Color = Color::Rgb(12, 14, 20);
const COLOR_BORDER: Color = Color::Rgb(84, 90, 108);
const COLOR_TEXT: Color = Color::Rgb(236, 238, 244);
const COLOR_MUTED: Color = Color::Rgb(150, 158, 180);
const COLOR_HEADER_BG: Color = Color::Rgb(34, 39, 52);
const COLOR_ROW_A: Color = Color::Rgb(20, 24, 33);
const COLOR_ROW_B: Color = Color::Rgb(16, 20, 29);
const COLOR_ROW_SELECTED: Color = Color::Rgb(42, 51, 71);
const COLOR_TRACK: Color = Color::Rgb(82, 86, 98);
const COLOR_SEPARATOR: Color = Color::Rgb(98, 106, 127);
const COLOR_ACCENT_CPU: Color = Color::Rgb(255, 186, 92);
const COLOR_ACCENT_THREAD: Color = Color::Rgb(255, 139, 72);
const COLOR_ACCENT_GPU: Color = Color::Rgb(255, 169, 90);
const COLOR_ACCENT_VRAM: Color = Color::Rgb(255, 205, 124);
const COLOR_ACCENT_PROC: Color = Color::Rgb(255, 191, 101);
const COLOR_OK: Color = Color::Rgb(255, 214, 130);
const COLOR_WARN: Color = Color::Rgb(255, 163, 94);
const COLOR_HOT: Color = Color::Rgb(255, 111, 111);
const MAX_CONTENT_WIDTH: u16 = 160;
const CPU_LAYOUT_SLACK_ROWS: u16 = 0;

#[derive(Debug, Clone)]
struct ProcessStat {
    pid: String,
    name: String,
    mem_mib: u64,
    cpu: f32,
}

#[derive(Debug, Clone)]
struct NvidiaStats {
    name: String,
    usage_pct: f32,
    temp_c: f32,
    vram_used_mib: f32,
    vram_total_mib: f32,
}

#[derive(Debug, Clone)]
struct StorageStats {
    used_bytes: u64,
    total_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
enum KillSignal {
    Term,
    Kill,
}

impl KillSignal {
    fn as_kill_arg(self) -> &'static str {
        match self {
            Self::Term => "-TERM",
            Self::Kill => "-KILL",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }
}

#[derive(Debug, Clone)]
struct StatusMessage {
    text: String,
    is_error: bool,
    expires_at: Instant,
}

#[derive(Debug)]
struct App {
    started_at: Instant,
    system: System,
    components: Components,
    cpu_usage_by_id: BTreeMap<usize, f32>,
    cpu_pairs: Vec<(usize, Option<usize>)>,
    cpu_model: String,
    cpu_temp_c: Option<f32>,
    storage: Option<StorageStats>,
    processes: Vec<ProcessStat>,
    nvidia: Option<NvidiaStats>,
    selected_index: usize,
    selected_pid: Option<String>,
    has_user_navigated_processes: bool,
    process_freeze_until: Option<Instant>,
    kill_menu_open: bool,
    status_message: Option<StatusMessage>,
}

impl App {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            system: System::new_all(),
            components: Components::new_with_refreshed_list(),
            cpu_usage_by_id: BTreeMap::new(),
            cpu_pairs: Vec::new(),
            cpu_model: String::new(),
            cpu_temp_c: None,
            storage: None,
            processes: Vec::new(),
            nvidia: None,
            selected_index: 0,
            selected_pid: None,
            has_user_navigated_processes: false,
            process_freeze_until: None,
            kill_menu_open: false,
            status_message: None,
        }
    }

    fn refresh_cpu(&mut self) {
        self.system.refresh_cpu_all();
        self.components.refresh(false);
        if self.cpu_model.is_empty() {
            self.cpu_model = detect_cpu_model(&self.system);
        }
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

    fn refresh_processes(&mut self) {
        self.system.refresh_memory();
        self.system.refresh_processes(
            ProcessesToUpdate::All,
            /* remove_dead_processes */ true,
        );
        self.processes = top_processes(&self.system, 256);
        self.sync_selection();
    }

    fn refresh_gpu(&mut self) {
        self.nvidia = read_nvidia_smi();
    }

    fn refresh_storage(&mut self) {
        self.storage = read_root_storage();
    }

    fn refresh_all(&mut self) {
        self.refresh_cpu();
        self.refresh_processes();
        self.refresh_gpu();
        self.refresh_storage();
    }

    fn sync_selection(&mut self) {
        if self.processes.is_empty() {
            self.selected_index = 0;
            self.selected_pid = None;
            self.kill_menu_open = false;
            return;
        }

        if !self.has_user_navigated_processes {
            self.selected_index = 0;
            self.selected_pid = Some(self.processes[0].pid.clone());
            return;
        }

        if let Some(pid) = self.selected_pid.as_deref() {
            if let Some(idx) = self.processes.iter().position(|proc| proc.pid == pid) {
                self.selected_index = idx;
                return;
            }
        }

        self.selected_index = self.selected_index.min(self.processes.len() - 1);
        self.selected_pid = Some(self.processes[self.selected_index].pid.clone());
    }

    fn selected_index(&self) -> Option<usize> {
        if self.processes.is_empty() {
            None
        } else {
            Some(self.selected_index)
        }
    }

    fn selected_process(&self) -> Option<&ProcessStat> {
        self.processes.get(self.selected_index)
    }

    fn move_selection(&mut self, delta: isize, now: Instant) {
        if self.processes.is_empty() {
            return;
        }

        self.has_user_navigated_processes = true;
        let max_idx = self.processes.len().saturating_sub(1);
        let next = if delta.is_negative() {
            self.selected_index.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected_index
                .saturating_add(delta as usize)
                .min(max_idx)
        };

        self.selected_index = next;
        self.selected_pid = Some(self.processes[self.selected_index].pid.clone());
        self.freeze_process_list(now);
    }

    fn freeze_process_list(&mut self, now: Instant) {
        self.process_freeze_until = Some(now + PROCESS_FREEZE_AFTER_NAV);
    }

    fn process_updates_frozen(&self, now: Instant) -> bool {
        self.process_freeze_until.is_some_and(|until| now < until)
    }

    fn process_updates_locked(&self, now: Instant) -> bool {
        self.kill_menu_open || self.process_updates_frozen(now)
    }

    fn process_freeze_remaining(&self, now: Instant) -> Option<Duration> {
        self.process_freeze_until
            .and_then(|until| (until > now).then(|| until.duration_since(now)))
    }

    fn open_kill_menu(&mut self, now: Instant) {
        if self.selected_process().is_some() {
            self.kill_menu_open = true;
            self.freeze_process_list(now);
        }
    }

    fn set_status(&mut self, now: Instant, message: impl Into<String>, is_error: bool) {
        self.status_message = Some(StatusMessage {
            text: message.into(),
            is_error,
            expires_at: now + STATUS_MESSAGE_DURATION,
        });
    }

    fn clear_expired_state(&mut self, now: Instant) {
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

    fn send_kill_to_selected(&self, signal: KillSignal) -> Result<String> {
        let pid = self
            .selected_process()
            .map(|proc| proc.pid.clone())
            .ok_or_else(|| anyhow!("No process selected"))?;

        send_signal_to_pid(&pid, signal)?;
        Ok(pid)
    }
}

fn pick_cpu_temp(components: &Components) -> Option<f32> {
    let priority = ["tctl", "tdie", "package", "cpu"];
    let mut best: Option<(usize, f32)> = None;

    for component in components.iter() {
        let Some(temp) = component.temperature() else {
            continue;
        };
        let label = component.label().to_ascii_lowercase();
        let rank = priority
            .iter()
            .position(|needle| label.contains(needle))
            .unwrap_or(usize::MAX);
        match best {
            None => best = Some((rank, temp)),
            Some((best_rank, _)) if rank < best_rank => best = Some((rank, temp)),
            _ => {}
        }
    }

    best.map(|(_, temp)| temp)
}

fn top_processes(system: &System, limit: usize) -> Vec<ProcessStat> {
    let logical_cpus = system.cpus().len().max(1) as f32;
    let mut rows: Vec<ProcessStat> = system
        .processes()
        .iter()
        .map(|(pid, process)| ProcessStat {
            pid: pid.to_string(),
            name: process.name().to_string_lossy().to_string(),
            mem_mib: process.memory() / (1024 * 1024),
            // Normalize per-process CPU by logical CPU count to match tools
            // that report usage against total system capacity (e.g. btop).
            cpu: (process.cpu_usage() / logical_cpus).clamp(0.0, 100.0),
        })
        .collect();

    rows.sort_by(|a, b| {
        b.cpu
            .partial_cmp(&a.cpu)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.mem_mib.cmp(&a.mem_mib))
    });
    rows.truncate(limit);
    rows
}

fn send_signal_to_pid(pid: &str, signal: KillSignal) -> Result<()> {
    let output = Command::new("kill")
        .args([signal.as_kill_arg(), pid])
        .output()?;
    if output.status.success() {
        return Ok(());
    }

    let error_text = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if error_text.is_empty() {
        return Err(anyhow!("kill failed with status {}", output.status));
    }
    Err(anyhow!("kill failed: {error_text}"))
}

fn read_nvidia_smi() -> Option<NvidiaStats> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,utilization.gpu,temperature.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let output_text = String::from_utf8(output.stdout).ok()?;
    let line = output_text.lines().next()?.trim();
    let mut fields = line.split(',').map(str::trim);
    Some(NvidiaStats {
        name: fields.next()?.to_string(),
        usage_pct: fields.next()?.parse::<f32>().ok()?,
        temp_c: fields.next()?.parse::<f32>().ok()?,
        vram_used_mib: fields.next()?.parse::<f32>().ok()?,
        vram_total_mib: fields.next()?.parse::<f32>().ok()?,
    })
}

fn read_root_storage() -> Option<StorageStats> {
    let output = Command::new("df")
        .args(["-B1", "--output=size,used", "/"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    let mut lines = text.lines();
    let _header = lines.next()?;
    let values = lines.next()?.split_whitespace().collect::<Vec<_>>();
    if values.len() < 2 {
        return None;
    }

    let total_bytes = values[0].parse::<u64>().ok()?;
    let used_bytes = values[1].parse::<u64>().ok()?.min(total_bytes);
    Some(StorageStats {
        used_bytes,
        total_bytes,
    })
}

fn parse_logical_cpu_id(name: &str, fallback: usize) -> usize {
    let mut digits = String::new();
    for ch in name.chars().rev() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            break;
        }
    }
    if digits.is_empty() {
        return fallback;
    }
    let value = digits.chars().rev().collect::<String>();
    value.parse::<usize>().unwrap_or(fallback)
}

fn pair_entries_count(pairs: &[(usize, Option<usize>)]) -> usize {
    pairs
        .iter()
        .map(|(_, right)| 1 + usize::from(right.is_some()))
        .sum()
}

fn detect_cpu_pairs(ids: Vec<usize>) -> Vec<(usize, Option<usize>)> {
    let mut sorted_ids = ids;
    sorted_ids.sort_unstable();

    if let Some(pairs) = detect_pairs_from_sysfs(&sorted_ids) {
        return pairs;
    }

    fallback_pairs(&sorted_ids)
}

fn detect_pairs_from_sysfs(sorted_ids: &[usize]) -> Option<Vec<(usize, Option<usize>)>> {
    let idset: BTreeSet<usize> = sorted_ids.iter().copied().collect();
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();

    for id in sorted_ids {
        let path = format!("/sys/devices/system/cpu/cpu{id}/topology/thread_siblings_list");
        let text = fs::read_to_string(path).ok()?;
        let mut siblings = parse_cpu_id_list(text.trim());
        siblings.retain(|cpu| idset.contains(cpu));
        siblings.sort_unstable();
        siblings.dedup();
        if siblings.is_empty() {
            continue;
        }
        let key = siblings[0];
        if let Some(existing) = groups.get_mut(&key) {
            for sibling in siblings {
                if !existing.contains(&sibling) {
                    existing.push(sibling);
                }
            }
            existing.sort_unstable();
        } else {
            groups.insert(key, siblings);
        }
    }

    if groups.is_empty() {
        return None;
    }

    let mut covered = BTreeSet::new();
    let mut out = Vec::new();
    for (_, group) in groups {
        if group.is_empty() {
            continue;
        }
        let core = group[0];
        let sibling = group.get(1).copied();
        out.push((core, sibling));
        covered.insert(core);
        if let Some(sibling) = sibling {
            covered.insert(sibling);
        }
        for extra in group.iter().skip(2) {
            if covered.insert(*extra) {
                out.push((*extra, None));
            }
        }
    }

    for id in sorted_ids {
        if covered.insert(*id) {
            out.push((*id, None));
        }
    }

    out.sort_unstable_by_key(|(left, _)| *left);
    Some(out)
}

fn fallback_pairs(sorted_ids: &[usize]) -> Vec<(usize, Option<usize>)> {
    if sorted_ids.len() >= 2 && sorted_ids.len().is_multiple_of(2) {
        let half = sorted_ids.len() / 2;
        let mut out = Vec::with_capacity(half);
        for idx in 0..half {
            out.push((sorted_ids[idx], Some(sorted_ids[idx + half])));
        }
        return out;
    }

    let mut out = Vec::new();
    let mut idx = 0;
    while idx < sorted_ids.len() {
        let left = sorted_ids[idx];
        let right = sorted_ids.get(idx + 1).copied();
        out.push((left, right));
        idx += 2;
    }
    out
}

fn parse_cpu_id_list(raw: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for part in raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if let Some((left, right)) = part.split_once('-') {
            let Ok(start) = left.parse::<usize>() else {
                continue;
            };
            let Ok(end) = right.parse::<usize>() else {
                continue;
            };
            if start <= end {
                out.extend(start..=end);
            } else {
                out.extend(end..=start);
            }
            continue;
        }
        if let Ok(cpu_id) = part.parse::<usize>() {
            out.push(cpu_id);
        }
    }
    out
}

fn detect_cpu_model(system: &System) -> String {
    let Some(cpu) = system.cpus().first() else {
        return "Unknown CPU".to_string();
    };

    let model = cpu.brand().trim();
    if !model.is_empty() {
        return model.to_string();
    }

    let fallback = cpu.name().trim();
    if !fallback.is_empty() {
        return fallback.to_string();
    }

    "Unknown CPU".to_string()
}

fn style_for_usage_with_base(usage: f32, base: Color) -> Style {
    if usage >= 90.0 {
        Style::default().fg(COLOR_HOT).add_modifier(Modifier::BOLD)
    } else if usage >= 70.0 {
        Style::default().fg(COLOR_WARN)
    } else if usage >= 40.0 {
        Style::default().fg(COLOR_OK)
    } else {
        Style::default().fg(base)
    }
}

fn style_for_temp(temp_c: Option<f32>) -> Style {
    match temp_c {
        Some(temp) if temp >= 85.0 => Style::default().fg(COLOR_HOT).add_modifier(Modifier::BOLD),
        Some(temp) if temp >= 70.0 => Style::default().fg(COLOR_WARN),
        Some(_) => Style::default().fg(COLOR_OK),
        None => Style::default().fg(COLOR_MUTED),
    }
}

fn braille_bar_segments(usage: f32, width: usize) -> (String, String) {
    if width == 0 {
        return (String::new(), String::new());
    }

    const BRAILLE_STEPS: [char; 8] = ['⡀', '⡄', '⡆', '⡇', '⣇', '⣧', '⣷', '⣿'];
    let total_units = ((usage.clamp(0.0, 100.0) / 100.0) * (width * 8) as f32).round() as usize;
    let full_cells = (total_units / 8).min(width);
    let partial_units = if full_cells < width {
        total_units % 8
    } else {
        0
    };
    let mut fill = String::with_capacity(width);
    fill.push_str(&"⣿".repeat(full_cells));
    if partial_units > 0 {
        fill.push(BRAILLE_STEPS[partial_units - 1]);
    }

    let used_cells = full_cells + usize::from(partial_units > 0);
    let track = "⠄".repeat(width.saturating_sub(used_cells));
    (fill, track)
}

fn cpu_lane_line(
    prefix: &str,
    logical_id: usize,
    usage: f32,
    width: usize,
    accent: Color,
) -> Line<'static> {
    let id = format!("{prefix}{logical_id:02}");
    let usage_text = format!("{:>5.1}%", usage);
    let fixed = id.len() + 1 + usage_text.len() + 1;
    let bar_width = width.saturating_sub(fixed);

    let mut spans = vec![
        Span::styled(
            format!("{id} "),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            usage_text,
            style_for_usage_with_base(usage, accent).add_modifier(Modifier::BOLD),
        ),
    ];

    if bar_width >= 6 {
        let (fill, track) = braille_bar_segments(usage, bar_width);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            fill,
            style_for_usage_with_base(usage, accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(track, Style::default().fg(COLOR_TRACK)));
    }

    Line::from(spans)
}

fn cpu_lane_placeholder(prefix: &str, width: usize, accent: Color) -> Line<'static> {
    let id = format!("{prefix}--");
    let fixed = id.len() + 1 + 3 + 1;
    let bar_width = width.saturating_sub(fixed);
    let mut spans = vec![
        Span::styled(
            format!("{id} "),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled("n/a", Style::default().fg(COLOR_MUTED)),
    ];

    if bar_width >= 6 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            "⠄".repeat(bar_width),
            Style::default().fg(COLOR_TRACK),
        ));
    }
    Line::from(spans)
}

fn cpu_pair_narrow_line(
    core_id: usize,
    core_usage: f32,
    smt_id: Option<usize>,
    smt_usage: Option<f32>,
    width: usize,
) -> Line<'static> {
    let core_prefix = format!("C{core_id:02} {:>4.0}%", core_usage);
    let mut spans = vec![Span::styled(
        core_prefix.clone(),
        style_for_usage_with_base(core_usage, COLOR_ACCENT_CPU).add_modifier(Modifier::BOLD),
    )];

    if let Some(thread_id) = smt_id {
        let thread_prefix = if let Some(thread_usage) = smt_usage {
            format!("T{thread_id:02} {:>4.0}%", thread_usage)
        } else {
            format!("T{thread_id:02}  n/a")
        };
        let fixed = core_prefix.len() + thread_prefix.len() + 3;
        let total_bar_width = width.saturating_sub(fixed);
        let core_bar_width = total_bar_width / 2;
        let thread_bar_width = total_bar_width.saturating_sub(core_bar_width);

        if core_bar_width >= 2 {
            let (fill, track) = braille_bar_segments(core_usage, core_bar_width);
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                fill,
                style_for_usage_with_base(core_usage, COLOR_ACCENT_CPU),
            ));
            spans.push(Span::styled(track, Style::default().fg(COLOR_TRACK)));
        }

        spans.push(Span::styled("│", Style::default().fg(COLOR_SEPARATOR)));

        if let Some(thread_usage) = smt_usage {
            spans.push(Span::styled(
                thread_prefix,
                style_for_usage_with_base(thread_usage, COLOR_ACCENT_THREAD)
                    .add_modifier(Modifier::BOLD),
            ));
            if thread_bar_width >= 2 {
                let (fill, track) = braille_bar_segments(thread_usage, thread_bar_width);
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    fill,
                    style_for_usage_with_base(thread_usage, COLOR_ACCENT_THREAD),
                ));
                spans.push(Span::styled(track, Style::default().fg(COLOR_TRACK)));
            }
        } else {
            spans.push(Span::styled(
                thread_prefix,
                Style::default().fg(COLOR_MUTED),
            ));
            if thread_bar_width >= 2 {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    "⠄".repeat(thread_bar_width),
                    Style::default().fg(COLOR_TRACK),
                ));
            }
        }
    } else {
        let fixed = core_prefix.len() + 1;
        let bar_width = width.saturating_sub(fixed);
        if bar_width >= 4 {
            let (fill, track) = braille_bar_segments(core_usage, bar_width);
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                fill,
                style_for_usage_with_base(core_usage, COLOR_ACCENT_CPU),
            ));
            spans.push(Span::styled(track, Style::default().fg(COLOR_TRACK)));
        }
    }

    Line::from(spans)
}

fn usage_meter_line(label: &str, usage: f32, width: usize, accent: Color) -> Line<'static> {
    let tag = format!("{label:<4}");
    let usage_text = format!("{:>5.1}%", usage);
    let fixed = tag.len() + usage_text.len() + 3;
    let bar_width = width.saturating_sub(fixed);

    let mut spans = vec![
        Span::styled(
            tag,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            usage_text,
            style_for_usage_with_base(usage, accent).add_modifier(Modifier::BOLD),
        ),
    ];
    if bar_width >= 8 {
        let (fill, track) = braille_bar_segments(usage, bar_width);
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            fill,
            style_for_usage_with_base(usage, accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(track, Style::default().fg(COLOR_TRACK)));
    }
    Line::from(spans)
}

fn value_usage_line(value_text: &str, usage: f32, accent: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            value_text.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{usage:.1}%"),
            style_for_usage_with_base(usage, accent).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn usage_bar_line(usage: f32, width: usize, accent: Color) -> Line<'static> {
    if width < 4 {
        return Line::from("");
    }
    let (fill, track) = braille_bar_segments(usage, width);
    Line::from(vec![
        Span::styled(
            fill,
            style_for_usage_with_base(usage, accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(track, Style::default().fg(COLOR_TRACK)),
    ])
}

fn truncate_ascii(input: &str, max_width: usize) -> String {
    let char_len = input.chars().count();
    if char_len <= max_width {
        return input.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let prefix = input.chars().take(max_width - 3).collect::<String>();
    format!("{prefix}...")
}

fn bytes_to_gb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

fn bytes_to_t(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0 * 1024.0)
}

fn mib_to_gb(mib: f32) -> f32 {
    mib / 1024.0
}

fn main() -> Result<()> {
    run_tui()
}

fn run_tui() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = ui_loop(&mut terminal);

    restore_terminal(terminal)?;
    result
}

fn restore_terminal(
    mut terminal: Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn ui_loop(terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>) -> Result<()> {
    let mut last_draw = Instant::now() - DRAW_INTERVAL;
    let mut last_cpu = Instant::now() - CPU_INTERVAL;
    let mut last_process = Instant::now() - PROCESS_INTERVAL;
    let mut last_gpu = Instant::now() - GPU_INTERVAL;
    let mut last_storage = Instant::now() - STORAGE_INTERVAL;
    let mut app = App::new();
    app.refresh_all();

    loop {
        let now = Instant::now();
        app.clear_expired_state(now);

        if now.duration_since(last_cpu) >= CPU_INTERVAL {
            app.refresh_cpu();
            last_cpu = now;
        }

        if now.duration_since(last_process) >= PROCESS_INTERVAL && !app.process_updates_locked(now)
        {
            app.refresh_processes();
            last_process = now;
        }

        if now.duration_since(last_gpu) >= GPU_INTERVAL {
            app.refresh_gpu();
            last_gpu = now;
        }

        if now.duration_since(last_storage) >= STORAGE_INTERVAL {
            app.refresh_storage();
            last_storage = now;
        }

        if now.duration_since(last_draw) >= DRAW_INTERVAL {
            terminal.draw(|f| draw_ui(f, &app, now))?;
            last_draw = now;
        }

        let process_due = if app.kill_menu_open {
            last_process + PROCESS_INTERVAL
        } else if let Some(until) = app.process_freeze_until {
            if now < until {
                until
            } else {
                last_process + PROCESS_INTERVAL
            }
        } else {
            last_process + PROCESS_INTERVAL
        };

        let next_due = (last_draw + DRAW_INTERVAL)
            .min(last_cpu + CPU_INTERVAL)
            .min(process_due)
            .min(last_gpu + GPU_INTERVAL)
            .min(last_storage + STORAGE_INTERVAL);
        let timeout = next_due.saturating_duration_since(Instant::now());

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Release {
                    continue;
                }

                let now = Instant::now();
                app.clear_expired_state(now);
                let mut redraw_now = false;

                if app.kill_menu_open {
                    match key.code {
                        KeyCode::Esc => {
                            app.kill_menu_open = false;
                            redraw_now = true;
                        }
                        KeyCode::Enter | KeyCode::Char('k') => {
                            app.kill_menu_open = false;
                            redraw_now = true;
                            match app.send_kill_to_selected(KillSignal::Term) {
                                Ok(pid) => {
                                    app.set_status(
                                        now,
                                        format!("{} sent to PID {pid}", KillSignal::Term.label()),
                                        false,
                                    );
                                    app.refresh_processes();
                                    last_process = now;
                                }
                                Err(err) => {
                                    app.set_status(now, err.to_string(), true);
                                }
                            }
                        }
                        KeyCode::Char('f') | KeyCode::Char('K') => {
                            app.kill_menu_open = false;
                            redraw_now = true;
                            match app.send_kill_to_selected(KillSignal::Kill) {
                                Ok(pid) => {
                                    app.set_status(
                                        now,
                                        format!("{} sent to PID {pid}", KillSignal::Kill.label()),
                                        false,
                                    );
                                    app.refresh_processes();
                                    last_process = now;
                                }
                                Err(err) => {
                                    app.set_status(now, err.to_string(), true);
                                }
                            }
                        }
                        _ => {}
                    }
                    if redraw_now {
                        let draw_now = Instant::now();
                        terminal.draw(|f| draw_ui(f, &app, draw_now))?;
                        last_draw = draw_now;
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Up => {
                        app.move_selection(-1, now);
                        redraw_now = true;
                    }
                    KeyCode::Down => {
                        app.move_selection(1, now);
                        redraw_now = true;
                    }
                    KeyCode::Enter => {
                        app.open_kill_menu(now);
                        redraw_now = true;
                    }
                    _ => {}
                }

                if redraw_now {
                    let draw_now = Instant::now();
                    terminal.draw(|f| draw_ui(f, &app, draw_now))?;
                    last_draw = draw_now;
                }
            }
        }
    }

    Ok(())
}

fn draw_ui(f: &mut Frame<'_>, app: &App, now: Instant) {
    f.render_widget(
        Block::default().style(Style::default().bg(COLOR_BG).fg(COLOR_TEXT)),
        f.area(),
    );

    let content = centered_content_area(f.area(), MAX_CONTENT_WIDTH);

    let base_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Percentage(40),
            Constraint::Length(7),
            Constraint::Min(7),
            Constraint::Length(1),
        ])
        .split(content);

    let cpu_target = app
        .cpu_pairs
        .len()
        .min(u16::MAX as usize)
        .try_into()
        .unwrap_or(u16::MAX)
        .saturating_add(3)
        .saturating_add(CPU_LAYOUT_SLACK_ROWS);

    let cpu_max_for_area = content.height.saturating_sub(1 + 7 + 1 + 7);
    let should_compact_cpu = cpu_max_for_area > 0 && base_layout[1].height > cpu_target;

    let layout = if should_compact_cpu {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(cpu_target.min(cpu_max_for_area)),
                Constraint::Length(7),
                Constraint::Min(7),
                Constraint::Length(1),
            ])
            .split(content)
    } else {
        base_layout
    };

    let middle = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(2, 3), Constraint::Ratio(1, 3)])
        .split(layout[2]);

    render_telemetry_banner(f, layout[0], app, now);
    render_cpu_grid(f, layout[1], app);
    render_gpu_panel(f, middle[0], app.nvidia.as_ref());
    render_mem_disk_panel(f, middle[1], app);
    render_process_table(f, layout[3], app, now);
    render_footer(f, layout[4], app, now);
    if app.kill_menu_open {
        render_kill_menu(f, app);
    }
}

fn render_telemetry_banner(f: &mut Frame<'_>, area: Rect, app: &App, now: Instant) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let tick = (now.duration_since(app.started_at).as_millis() / 120) as usize;
    let bus_line = animated_title_line(area.width as usize, tick, " RTOP ", false);
    f.render_widget(
        Paragraph::new(bus_line).style(Style::default().bg(COLOR_ROW_B)),
        area,
    );
}

fn animated_title_line(width: usize, tick: usize, title: &str, dense: bool) -> Line<'static> {
    if width == 0 {
        return Line::from("");
    }

    let sparse_pattern: [char; 6] = ['·', '·', ' ', '·', '•', ' '];
    let dense_pattern: [char; 8] = ['┄', '·', '┈', ' ', '·', '┈', ' ', '┄'];
    let base_pattern: &[char] = if dense {
        &dense_pattern[..]
    } else {
        &sparse_pattern[..]
    };

    let mut chars = vec![' '; width];
    let mut colors = vec![COLOR_TRACK; width];
    let mut bold = vec![false; width];
    for idx in 0..width {
        chars[idx] = base_pattern[idx % base_pattern.len()];
    }

    let title_chars = title.chars().collect::<Vec<_>>();
    let title_len = title_chars.len().min(width);
    let title_start = if title_len > 0 {
        (width - title_len) / 2
    } else {
        0
    };
    let title_end = title_start.saturating_add(title_len);

    let pulse_count = if dense { 6 } else { 4 };
    for lane in 0..pulse_count {
        let pos = ((lane + 1) * width) / (pulse_count + 1);
        if pos >= title_start && pos < title_end {
            continue;
        }
        let phase = (tick + lane * 3) % 18;
        if phase == 0 {
            chars[pos] = if dense { '◆' } else { '●' };
            colors[pos] = match lane % 4 {
                0 => COLOR_ACCENT_CPU,
                1 => COLOR_ACCENT_GPU,
                2 => COLOR_ACCENT_VRAM,
                _ => COLOR_ACCENT_PROC,
            };
            bold[pos] = true;
        } else if phase <= 2 {
            chars[pos] = if dense { '◦' } else { '•' };
            colors[pos] = COLOR_SEPARATOR;
        }
    }

    if title_len > 0 {
        for (offset, ch) in title_chars.iter().take(title_len).enumerate() {
            let pos = title_start + offset;
            chars[pos] = *ch;
            colors[pos] = COLOR_TEXT;
            bold[pos] = true;
        }
    }

    let mut spans = Vec::with_capacity(width);
    for idx in 0..width {
        let style = if bold[idx] {
            Style::default()
                .fg(colors[idx])
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(colors[idx])
        };
        spans.push(Span::styled(chars[idx].to_string(), style));
    }
    Line::from(spans)
}

fn render_cpu_grid(f: &mut Frame<'_>, area: Rect, app: &App) {
    let title_width = area.width.saturating_sub(4) as usize;
    let title_text = truncate_ascii(&format!("CPU {}", app.cpu_model), title_width);
    let block = Block::default()
        .title(
            Line::from(format!(" {title_text} ")).centered().style(
                Style::default()
                    .fg(COLOR_ACCENT_CPU)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(COLOR_BORDER));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.cpu_usage_by_id.is_empty()
        || app.cpu_pairs.is_empty()
        || inner.width < 28
        || inner.height < 2
    {
        return;
    }

    let avg = app.cpu_usage_by_id.values().copied().sum::<f32>() / app.cpu_usage_by_id.len() as f32;
    let temp = app
        .cpu_temp_c
        .map(|v| format!("{v:.0}C"))
        .unwrap_or_else(|| "n/a".to_string());

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    let rows = chunks[1].height as usize;
    if rows == 0 {
        return;
    }
    let shown = rows.min(app.cpu_pairs.len());
    let summary = Paragraph::new(Line::from(vec![
        Span::styled("Total ", Style::default().fg(COLOR_MUTED)),
        Span::styled(
            format!("{:>5.1}%", avg),
            style_for_usage_with_base(avg, COLOR_ACCENT_CPU).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | Temp ", Style::default().fg(COLOR_MUTED)),
        Span::styled(
            temp,
            style_for_temp(app.cpu_temp_c).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | Threads ", Style::default().fg(COLOR_MUTED)),
        Span::styled(
            format!("{}", app.cpu_usage_by_id.len()),
            Style::default()
                .fg(COLOR_ACCENT_THREAD)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | Cores ", Style::default().fg(COLOR_MUTED)),
        Span::styled(
            format!("{}", app.cpu_pairs.len()),
            Style::default()
                .fg(COLOR_ACCENT_CPU)
                .add_modifier(Modifier::BOLD),
        ),
    ]))
    .style(Style::default().bg(COLOR_ROW_A));
    f.render_widget(summary, chunks[0]);

    let mut row_constraints = vec![Constraint::Length(1); shown];
    row_constraints.push(Constraint::Min(0));
    let row_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(chunks[1]);

    for (row_idx, row_area) in row_chunks.iter().take(shown).enumerate() {
        let (core_id, smt_id) = app.cpu_pairs[row_idx];
        let core_usage = app.cpu_usage_by_id.get(&core_id).copied().unwrap_or(0.0);
        let row_bg = if row_idx.is_multiple_of(2) {
            COLOR_ROW_A
        } else {
            COLOR_ROW_B
        };

        if row_area.width >= 52 {
            let col_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(50),
                    Constraint::Length(1),
                    Constraint::Percentage(49),
                ])
                .split(*row_area);

            let core_line = cpu_lane_line(
                "C",
                core_id,
                core_usage,
                col_chunks[0].width as usize,
                COLOR_ACCENT_CPU,
            );
            f.render_widget(
                Paragraph::new(core_line).style(Style::default().bg(row_bg)),
                col_chunks[0],
            );

            let divider =
                Paragraph::new(Line::from("│").style(Style::default().fg(COLOR_SEPARATOR)))
                    .style(Style::default().bg(row_bg));
            f.render_widget(divider, col_chunks[1]);

            let thread_line = if let Some(smt_id) = smt_id {
                let smt_usage = app.cpu_usage_by_id.get(&smt_id).copied().unwrap_or(0.0);
                cpu_lane_line(
                    "T",
                    smt_id,
                    smt_usage,
                    col_chunks[2].width as usize,
                    COLOR_ACCENT_THREAD,
                )
            } else {
                cpu_lane_placeholder("T", col_chunks[2].width as usize, COLOR_ACCENT_THREAD)
            };
            f.render_widget(
                Paragraph::new(thread_line).style(Style::default().bg(row_bg)),
                col_chunks[2],
            );
        } else {
            let smt_usage = smt_id.and_then(|id| app.cpu_usage_by_id.get(&id).copied());
            let compact = cpu_pair_narrow_line(
                core_id,
                core_usage,
                smt_id,
                smt_usage,
                row_area.width as usize,
            );
            f.render_widget(
                Paragraph::new(compact).style(Style::default().bg(row_bg)),
                *row_area,
            );
        }
    }
}

fn render_gpu_panel(f: &mut Frame<'_>, area: Rect, nvidia: Option<&NvidiaStats>) {
    let title_width = area.width.saturating_sub(4) as usize;
    let title_text = if let Some(stats) = nvidia {
        truncate_ascii(&format!("GPU {}", stats.name), title_width)
    } else {
        "GPU".to_string()
    };

    let outer = Block::default()
        .title(
            Line::from(format!(" {title_text} ")).centered().style(
                Style::default()
                    .fg(COLOR_ACCENT_GPU)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(COLOR_BORDER));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    if inner.height < 2 {
        return;
    }

    let Some(stats) = nvidia else {
        let note = Paragraph::new("No NVIDIA metrics (install/enable nvidia-smi)")
            .style(Style::default().fg(COLOR_MUTED));
        f.render_widget(note, inner);
        return;
    };

    let vram_used_gb = mib_to_gb(stats.vram_used_mib);
    let vram_total_gb = mib_to_gb(stats.vram_total_mib);

    let temp_line = Line::from(vec![
        Span::styled("Temp ", Style::default().fg(COLOR_MUTED)),
        Span::styled(
            format!("{:>4.0}C", stats.temp_c),
            style_for_temp(Some(stats.temp_c)).add_modifier(Modifier::BOLD),
        ),
    ]);
    let vram_detail_line = Line::from(vec![
        Span::styled("VRAM ", Style::default().fg(COLOR_MUTED)),
        Span::styled(
            format!("{vram_used_gb:.1}/{vram_total_gb:.1} GB"),
            Style::default()
                .fg(COLOR_ACCENT_VRAM)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let summary = Line::from(vec![
        Span::styled("Temp ", Style::default().fg(COLOR_MUTED)),
        Span::styled(
            format!("{:>4.0}C", stats.temp_c),
            style_for_temp(Some(stats.temp_c)).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | ", Style::default().fg(COLOR_MUTED)),
        Span::styled("VRAM ", Style::default().fg(COLOR_MUTED)),
        Span::styled(
            format!("{vram_used_gb:.1}/{vram_total_gb:.1} GB"),
            Style::default()
                .fg(COLOR_ACCENT_VRAM)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let vram_pct = if stats.vram_total_mib <= 0.0 {
        0.0
    } else {
        (stats.vram_used_mib / stats.vram_total_mib * 100.0).clamp(0.0, 100.0)
    };

    if inner.height >= 5 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);
        f.render_widget(
            Paragraph::new(temp_line).style(Style::default().bg(COLOR_ROW_A)),
            chunks[0],
        );

        let usage = stats.usage_pct.clamp(0.0, 100.0);
        let usage_line = usage_meter_line("GPU", usage, chunks[1].width as usize, COLOR_ACCENT_GPU);
        f.render_widget(
            Paragraph::new(usage_line).style(Style::default().bg(COLOR_ROW_B)),
            chunks[1],
        );

        let separator = Paragraph::new("─".repeat(chunks[2].width as usize))
            .style(Style::default().fg(COLOR_SEPARATOR).bg(COLOR_ROW_A));
        f.render_widget(separator, chunks[2]);

        let vram_line = usage_meter_line(
            "VRAM",
            vram_pct,
            chunks[3].width as usize,
            COLOR_ACCENT_VRAM,
        );
        f.render_widget(
            Paragraph::new(vram_line).style(Style::default().bg(COLOR_ROW_B)),
            chunks[3],
        );
        f.render_widget(
            Paragraph::new(vram_detail_line).style(Style::default().bg(COLOR_ROW_A)),
            chunks[4],
        );
    } else if inner.height >= 4 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);
        f.render_widget(
            Paragraph::new(summary).style(Style::default().bg(COLOR_ROW_A)),
            chunks[0],
        );

        let usage = stats.usage_pct.clamp(0.0, 100.0);
        let usage_line = usage_meter_line("GPU", usage, chunks[1].width as usize, COLOR_ACCENT_GPU);
        f.render_widget(
            Paragraph::new(usage_line).style(Style::default().bg(COLOR_ROW_B)),
            chunks[1],
        );

        let separator = Paragraph::new("─".repeat(chunks[2].width as usize))
            .style(Style::default().fg(COLOR_SEPARATOR).bg(COLOR_ROW_A));
        f.render_widget(separator, chunks[2]);

        let vram_line = usage_meter_line(
            "VRAM",
            vram_pct,
            chunks[3].width as usize,
            COLOR_ACCENT_VRAM,
        );
        f.render_widget(
            Paragraph::new(vram_line).style(Style::default().bg(COLOR_ROW_B)),
            chunks[3],
        );
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(inner);
        f.render_widget(Paragraph::new(summary), chunks[0]);
    }
}

fn render_mem_disk_panel(f: &mut Frame<'_>, area: Rect, app: &App) {
    let outer = Block::default()
        .title(
            Line::from(" RAM ").centered().style(
                Style::default()
                    .fg(COLOR_ACCENT_PROC)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(COLOR_BORDER));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    if inner.height < 2 {
        return;
    }

    let ram_total_b = app.system.total_memory();
    let ram_used_b = app.system.used_memory().min(ram_total_b);
    let ram_pct = if ram_total_b == 0 {
        0.0
    } else {
        (ram_used_b as f32 / ram_total_b as f32 * 100.0).clamp(0.0, 100.0)
    };
    let ram_used_gb = bytes_to_gb(ram_used_b);
    let ram_total_gb = bytes_to_gb(ram_total_b);

    let (disk_used_t, disk_total_t, disk_pct) = if let Some(storage) = &app.storage {
        let disk_used_t = bytes_to_t(storage.used_bytes);
        let disk_total_t = bytes_to_t(storage.total_bytes);
        let disk_pct = if storage.total_bytes == 0 {
            0.0
        } else {
            (storage.used_bytes as f32 / storage.total_bytes as f32 * 100.0).clamp(0.0, 100.0)
        };
        (disk_used_t, disk_total_t, disk_pct)
    } else {
        (0.0, 0.0, 0.0)
    };

    if inner.height >= 5 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .split(inner);

        let ram_line = value_usage_line(
            &format!("{ram_used_gb:.1}/{ram_total_gb:.1} GB"),
            ram_pct,
            COLOR_ACCENT_GPU,
        );
        f.render_widget(
            Paragraph::new(ram_line).style(Style::default().bg(COLOR_ROW_A)),
            chunks[0],
        );

        let ram_bar = usage_bar_line(ram_pct, chunks[1].width as usize, COLOR_ACCENT_GPU);
        f.render_widget(
            Paragraph::new(ram_bar).style(Style::default().bg(COLOR_ROW_B)),
            chunks[1],
        );

        let line_width = chunks[2].width as usize;
        let hdd_label = " HDD ";
        let hdd_header_line = if line_width <= hdd_label.len() {
            Line::from(Span::styled(
                "HDD",
                Style::default()
                    .fg(COLOR_ACCENT_VRAM)
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            let side_width = line_width - hdd_label.len();
            let left = side_width / 2;
            let right = side_width - left;
            Line::from(vec![
                Span::styled("─".repeat(left), Style::default().fg(COLOR_SEPARATOR)),
                Span::styled(
                    hdd_label,
                    Style::default()
                        .fg(COLOR_ACCENT_VRAM)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("─".repeat(right), Style::default().fg(COLOR_SEPARATOR)),
            ])
        };
        f.render_widget(
            Paragraph::new(hdd_header_line).style(Style::default().bg(COLOR_ROW_A)),
            chunks[2],
        );

        let disk_line = value_usage_line(
            &if app.storage.is_some() {
                format!("{disk_used_t:.2}/{disk_total_t:.2} T")
            } else {
                "n/a".to_string()
            },
            disk_pct,
            COLOR_ACCENT_VRAM,
        );
        f.render_widget(
            Paragraph::new(disk_line).style(Style::default().bg(COLOR_ROW_B)),
            chunks[3],
        );

        let disk_bar = usage_bar_line(disk_pct, chunks[4].width as usize, COLOR_ACCENT_VRAM);
        f.render_widget(
            Paragraph::new(disk_bar).style(Style::default().bg(COLOR_ROW_A)),
            chunks[4],
        );
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(inner);

        let line_top = Line::from(vec![
            Span::styled(
                format!("{ram_used_gb:.1}/{ram_total_gb:.1} GB"),
                Style::default().fg(COLOR_ACCENT_GPU),
            ),
            Span::styled(
                format!(" {ram_pct:.1}%"),
                style_for_usage_with_base(ram_pct, COLOR_ACCENT_GPU),
            ),
        ]);
        f.render_widget(Paragraph::new(line_top), chunks[0]);

        let line_bottom = Line::from(vec![
            Span::styled(
                if app.storage.is_some() {
                    format!("{disk_used_t:.2}/{disk_total_t:.2} T")
                } else {
                    "n/a".to_string()
                },
                Style::default().fg(COLOR_ACCENT_VRAM),
            ),
            Span::styled(
                format!(" {disk_pct:.1}%"),
                style_for_usage_with_base(disk_pct, COLOR_ACCENT_VRAM),
            ),
        ]);
        f.render_widget(Paragraph::new(line_bottom), chunks[1]);
    }
}

fn render_process_table(f: &mut Frame<'_>, area: Rect, app: &App, now: Instant) {
    let selected_index = app.selected_index();
    let rows = app.processes.iter().enumerate().map(|(idx, p)| {
        let is_selected = selected_index.is_some_and(|selected| selected == idx);
        let row_bg = if is_selected {
            COLOR_ROW_SELECTED
        } else if idx.is_multiple_of(2) {
            COLOR_ROW_A
        } else {
            COLOR_ROW_B
        };
        Row::new(vec![
            Cell::from(p.pid.clone()).style(
                Style::default()
                    .fg(if is_selected {
                        COLOR_TEXT
                    } else {
                        COLOR_ACCENT_PROC
                    })
                    .bg(row_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Cell::from(p.name.clone()).style(
                Style::default()
                    .fg(COLOR_TEXT)
                    .bg(row_bg)
                    .add_modifier(if is_selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Cell::from(format!("{} MiB", p.mem_mib)).style(
                Style::default()
                    .fg(COLOR_ACCENT_VRAM)
                    .bg(row_bg)
                    .add_modifier(if is_selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Cell::from(format!("{:.1}%", p.cpu)).style(
                style_for_usage_with_base(p.cpu, COLOR_ACCENT_THREAD)
                    .bg(row_bg)
                    .add_modifier(if is_selected {
                        Modifier::BOLD | Modifier::UNDERLINED
                    } else {
                        Modifier::BOLD
                    }),
            ),
        ])
    });

    let title = if app.process_updates_locked(now) {
        " Processes (frozen) "
    } else {
        " Processes "
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Percentage(55),
            Constraint::Length(14),
            Constraint::Length(10),
        ],
    )
    .header(
        Row::new(vec!["PID", "Process", "Memory", "CPU"]).style(
            Style::default()
                .fg(COLOR_TEXT)
                .bg(COLOR_HEADER_BG)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .column_spacing(1)
    .block(
        Block::default()
            .title(
                Line::from(title).style(
                    Style::default()
                        .fg(COLOR_ACCENT_PROC)
                        .add_modifier(Modifier::BOLD),
                ),
            )
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(COLOR_BORDER)),
    );

    f.render_widget(table, area);
}

fn render_kill_menu(f: &mut Frame<'_>, app: &App) {
    let Some(process) = app.selected_process() else {
        return;
    };

    let popup = centered_rect(62, 26, f.area());
    f.render_widget(Clear, popup);
    let block = Block::default()
        .title(
            Line::from(" Process Action ").style(
                Style::default()
                    .fg(COLOR_ACCENT_PROC)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(COLOR_BORDER))
        .style(Style::default().bg(COLOR_ROW_A));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if inner.height < 3 {
        return;
    }

    let title_width = inner.width.saturating_sub(2) as usize;
    let name = truncate_ascii(&process.name, title_width.max(1));
    let lines = vec![
        Line::from(vec![
            Span::styled("PID ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                process.pid.clone(),
                Style::default()
                    .fg(COLOR_ACCENT_PROC)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", Style::default()),
            Span::styled("Name ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                name,
                Style::default().fg(COLOR_TEXT).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Enter / k",
                Style::default().fg(COLOR_OK).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" SIGTERM   ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                "f",
                Style::default().fg(COLOR_WARN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" SIGKILL   ", Style::default().fg(COLOR_MUTED)),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(COLOR_ACCENT_PROC)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" cancel", Style::default().fg(COLOR_MUTED)),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn centered_content_area(area: Rect, max_width: u16) -> Rect {
    if area.width <= max_width {
        return area;
    }

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width - max_width) / 2),
            Constraint::Length(max_width),
            Constraint::Min(0),
        ])
        .split(area);
    horizontal[1]
}

fn render_footer(f: &mut Frame<'_>, area: Rect, app: &App, now: Instant) {
    let mut spans = vec![
        Span::styled(
            "↑/↓",
            Style::default()
                .fg(COLOR_ACCENT_PROC)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" select  |  ", Style::default().fg(COLOR_MUTED)),
        Span::styled(
            "Enter",
            Style::default()
                .fg(COLOR_ACCENT_PROC)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" actions  |  ", Style::default().fg(COLOR_MUTED)),
        Span::styled(
            "q/Esc",
            Style::default()
                .fg(COLOR_ACCENT_PROC)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" quit", Style::default().fg(COLOR_MUTED)),
    ];

    if let Some(remaining) = app.process_freeze_remaining(now) {
        spans.push(Span::styled("  |  ", Style::default().fg(COLOR_MUTED)));
        spans.push(Span::styled(
            format!("list frozen {:.1}s", remaining.as_secs_f32()),
            Style::default().fg(COLOR_OK).add_modifier(Modifier::BOLD),
        ));
    }

    if app.kill_menu_open {
        spans.push(Span::styled("  |  ", Style::default().fg(COLOR_MUTED)));
        spans.push(Span::styled(
            "action menu open",
            Style::default().fg(COLOR_WARN).add_modifier(Modifier::BOLD),
        ));
    }

    if let Some(status) = &app.status_message {
        let color = if status.is_error { COLOR_HOT } else { COLOR_OK };
        spans.push(Span::styled("  |  ", Style::default().fg(COLOR_MUTED)));
        spans.push(Span::styled("status ", Style::default().fg(COLOR_MUTED)));
        spans.push(Span::styled(
            status.text.clone(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
