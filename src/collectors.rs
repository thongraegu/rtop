use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use sysinfo::{Components, System};

use crate::models::{GpuStats, KillSignal, ProcessStat, StorageStats};

pub fn pick_cpu_temp(components: &Components) -> Option<f32> {
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

pub fn top_processes(system: &System, limit: usize) -> Vec<ProcessStat> {
    let logical_cpus = system.cpus().len().max(1) as f32;
    let mut rows: Vec<ProcessStat> = system
        .processes()
        .iter()
        // On Linux, sysinfo exposes tasks/threads as process entries as well.
        // Keep only thread-group leaders so process counts and memory totals
        // align with standard "process list" expectations.
        .filter(|(_, process)| process.thread_kind().is_none())
        .map(|(pid, process)| {
            let pid = pid.to_string();
            ProcessStat {
                app: detect_process_app_name(process),
                pid: pid.clone(),
                name: process.name().to_string_lossy().to_string(),
                mem_mib: process_mem_mib(&pid, process),
                // Normalize per-process CPU by logical CPU count to match tools
                // that report usage against total system capacity (e.g. btop).
                cpu: (process.cpu_usage() / logical_cpus).clamp(0.0, 100.0),
            }
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

fn process_mem_mib(pid: &str, process: &sysinfo::Process) -> u64 {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if let Some(pss_kib) = read_pss_kib(pid) {
        return pss_kib / 1024;
    }

    process.memory() / (1024 * 1024)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_pss_kib(pid: &str) -> Option<u64> {
    let content = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    for line in content.lines() {
        let Some(rest) = line.strip_prefix("Pss:") else {
            continue;
        };
        let kib = rest.split_whitespace().next()?.parse::<u64>().ok()?;
        return Some(kib);
    }
    None
}

fn detect_process_app_name(process: &sysinfo::Process) -> String {
    if let Some(name) = desktop_app_resolver().lookup_process(process) {
        return name;
    }

    if let Some(exe_name) = process
        .exe()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
    {
        let trimmed = exe_name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    if let Some(first_cmd) = process.cmd().first() {
        let first_cmd = first_cmd.to_string_lossy();
        if !first_cmd.is_empty() {
            if let Some(base) = Path::new(first_cmd.as_ref())
                .file_name()
                .and_then(|name| name.to_str())
            {
                let trimmed = base.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
            return first_cmd.to_string();
        }
    }

    let fallback = process.name().to_string_lossy();
    let fallback = fallback.trim();
    if fallback.is_empty() {
        "unknown".to_string()
    } else {
        fallback.to_string()
    }
}

#[derive(Debug, Default)]
struct DesktopAppResolver {
    by_exec: BTreeMap<String, String>,
    by_desktop_id: BTreeMap<String, String>,
}

impl DesktopAppResolver {
    fn from_system() -> Self {
        let mut resolver = Self::default();
        for desktop_file in list_desktop_files() {
            let Some((name, exec)) = parse_desktop_file(&desktop_file) else {
                continue;
            };

            let Some(stem) = desktop_file.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let stem_key = normalize_lookup_key(stem);
            resolver
                .by_desktop_id
                .entry(stem_key)
                .or_insert_with(|| name.clone());

            if let Some(exec_base) = exec.as_deref().and_then(extract_exec_basename) {
                resolver.by_exec.entry(exec_base).or_insert(name);
            }
        }

        resolver
    }

    fn lookup_process(&self, process: &sysinfo::Process) -> Option<String> {
        for token in process.cmd() {
            let token = token.to_string_lossy();
            if let Some(desktop_id) = extract_desktop_id_candidate(&token) {
                if let Some(name) = self.lookup_desktop_id(&desktop_id) {
                    return Some(name);
                }
            }
        }

        if let Some(exe_name) = process
            .exe()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
        {
            if let Some(name) = self.lookup_exec(exe_name) {
                return Some(name);
            }
        }

        if let Some(first_cmd) = process.cmd().first() {
            let first_cmd = first_cmd.to_string_lossy();
            if let Some(base) = Path::new(first_cmd.as_ref())
                .file_name()
                .and_then(|name| name.to_str())
            {
                if let Some(name) = self.lookup_exec(base) {
                    return Some(name);
                }
            }
            if let Some(name) = self.lookup_desktop_id(&first_cmd) {
                return Some(name);
            }
        }

        let process_name = process.name().to_string_lossy();
        if let Some(name) = self.lookup_exec(process_name.as_ref()) {
            return Some(name);
        }
        self.lookup_desktop_id(process_name.as_ref())
    }

    fn lookup_exec(&self, key: &str) -> Option<String> {
        self.by_exec.get(&normalize_lookup_key(key)).cloned()
    }

    fn lookup_desktop_id(&self, key: &str) -> Option<String> {
        self.by_desktop_id.get(&normalize_desktop_id(key)).cloned()
    }
}

fn desktop_app_resolver() -> &'static DesktopAppResolver {
    static RESOLVER: OnceLock<DesktopAppResolver> = OnceLock::new();
    RESOLVER.get_or_init(DesktopAppResolver::from_system)
}

fn list_desktop_files() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(xdg_data_home) = env::var("XDG_DATA_HOME") {
        roots.push(PathBuf::from(xdg_data_home).join("applications"));
    } else if let Ok(home) = env::var("HOME") {
        roots.push(PathBuf::from(home).join(".local/share/applications"));
    }

    roots.push(PathBuf::from("/usr/local/share/applications"));
    roots.push(PathBuf::from("/usr/share/applications"));
    roots.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    roots.push(PathBuf::from("/var/lib/snapd/desktop/applications"));

    let mut files = Vec::new();
    let mut stack = roots;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_some_and(|ext| ext == "desktop") {
                files.push(path);
            }
        }
    }
    files
}

fn parse_desktop_file(path: &Path) -> Option<(String, Option<String>)> {
    let text = fs::read_to_string(path).ok()?;
    let mut in_desktop_entry = false;
    let mut name = None;
    let mut exec = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line.eq_ignore_ascii_case("[Desktop Entry]");
            continue;
        }
        if !in_desktop_entry {
            continue;
        }

        if name.is_none() && line.starts_with("Name=") {
            let value = line.trim_start_matches("Name=").trim();
            if !value.is_empty() {
                name = Some(value.to_string());
            }
            continue;
        }

        if exec.is_none() && line.starts_with("Exec=") {
            let value = line.trim_start_matches("Exec=").trim();
            if !value.is_empty() {
                exec = Some(value.to_string());
            }
        }
    }

    Some((name?, exec))
}

fn extract_exec_basename(exec: &str) -> Option<String> {
    let mut parts = exec.split_whitespace().peekable();
    while let Some(part) = parts.next() {
        if part == "env" {
            continue;
        }
        if part.contains('=') && !part.contains('/') {
            continue;
        }

        let command = part.trim_matches(|ch| ch == '"' || ch == '\'');
        if command.is_empty() || command.starts_with('%') {
            continue;
        }
        let base = Path::new(command)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(command)
            .trim();
        if base.is_empty() {
            continue;
        }
        return Some(normalize_lookup_key(base));
    }
    None
}

fn extract_desktop_id_candidate(raw: &str) -> Option<String> {
    if !raw.contains(".desktop") {
        return None;
    }

    let token = raw.trim_matches(|ch| ch == '"' || ch == '\'');
    let token = token.strip_prefix("application://").unwrap_or(token);
    let start = token.rfind('/').map(|idx| idx + 1).unwrap_or(0);
    let tail = &token[start..];
    let end = tail.find(".desktop")?;
    let id = &tail[..end];
    if id.is_empty() {
        return None;
    }
    Some(id.to_string())
}

fn normalize_lookup_key(raw: &str) -> String {
    let key = raw.trim();
    let key = Path::new(key)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(key);
    key.to_ascii_lowercase()
}

fn normalize_desktop_id(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches(".desktop");
    let base = Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(trimmed);
    base.to_ascii_lowercase()
}

pub fn send_signal_to_pid(pid: &str, signal: KillSignal) -> Result<()> {
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

pub fn read_gpu_stats() -> Option<GpuStats> {
    read_nvidia_smi()
        .or_else(read_amd_gpu_sysfs)
        .or_else(read_intel_gpu_sysfs)
}

fn read_nvidia_smi() -> Option<GpuStats> {
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
    Some(GpuStats {
        name: fields.next()?.to_string(),
        usage_pct: fields.next()?.parse::<f32>().ok()?,
        temp_c: fields.next()?.parse::<f32>().ok()?,
        vram_used_mib: fields.next()?.parse::<f32>().ok()?,
        vram_total_mib: fields.next()?.parse::<f32>().ok()?,
    })
}

fn read_amd_gpu_sysfs() -> Option<GpuStats> {
    let entries = fs::read_dir("/sys/class/drm").ok()?;
    for entry in entries.flatten() {
        let card_name = entry.file_name();
        let card_name = card_name.to_string_lossy();
        if !is_drm_card_dir(&card_name) {
            continue;
        }

        let card_path = entry.path();
        let device_path = card_path.join("device");
        let Some(vendor) = read_trimmed_file(&device_path.join("vendor")) else {
            continue;
        };
        if vendor != "0x1002" {
            continue;
        }

        if let Some(stats) = read_amd_card_stats(&card_path, &device_path, &card_name) {
            return Some(stats);
        }
    }
    None
}

fn read_intel_gpu_sysfs() -> Option<GpuStats> {
    let entries = fs::read_dir("/sys/class/drm").ok()?;
    for entry in entries.flatten() {
        let card_name = entry.file_name();
        let card_name = card_name.to_string_lossy();
        if !is_drm_card_dir(&card_name) {
            continue;
        }

        let card_path = entry.path();
        let device_path = card_path.join("device");
        let Some(vendor) = read_trimmed_file(&device_path.join("vendor")) else {
            continue;
        };
        if vendor != "0x8086" {
            continue;
        }

        if let Some(stats) = read_intel_card_stats(&card_path, &device_path, &card_name) {
            return Some(stats);
        }
    }
    None
}

fn is_drm_card_dir(name: &str) -> bool {
    let Some(index) = name.strip_prefix("card") else {
        return false;
    };
    !index.is_empty() && index.chars().all(|ch| ch.is_ascii_digit())
}

fn read_amd_card_stats(card_path: &Path, device_path: &Path, card_name: &str) -> Option<GpuStats> {
    let usage_pct = read_f32_from_file(&device_path.join("gpu_busy_percent")).unwrap_or(0.0);
    let temp_c = read_hwmon_temp_c(device_path).unwrap_or(0.0);
    let vram_used_mib = read_u64_from_file(&device_path.join("mem_info_vram_used"))
        .map(bytes_to_mib)
        .unwrap_or(0.0);
    let vram_total_mib = read_u64_from_file(&device_path.join("mem_info_vram_total"))
        .map(bytes_to_mib)
        .unwrap_or(0.0);
    let vram_used_mib = if vram_total_mib > 0.0 {
        vram_used_mib.min(vram_total_mib)
    } else {
        vram_used_mib
    };

    if usage_pct <= 0.0 && temp_c <= 0.0 && vram_total_mib <= 0.0 {
        return None;
    }

    Some(GpuStats {
        name: detect_amd_gpu_name(card_path, device_path, card_name),
        usage_pct: usage_pct.clamp(0.0, 100.0),
        temp_c,
        vram_used_mib,
        vram_total_mib,
    })
}

fn read_intel_card_stats(
    card_path: &Path,
    device_path: &Path,
    card_name: &str,
) -> Option<GpuStats> {
    let usage_pct = read_intel_usage_pct(card_path, device_path).unwrap_or(0.0);
    let temp_c = read_hwmon_temp_c(device_path).unwrap_or(0.0);
    let (vram_used_mib, vram_total_mib) =
        read_intel_local_memory_mib(device_path).unwrap_or((0.0, 0.0));
    let vram_used_mib = if vram_total_mib > 0.0 {
        vram_used_mib.min(vram_total_mib)
    } else {
        vram_used_mib
    };

    Some(GpuStats {
        name: detect_intel_gpu_name(card_path, device_path, card_name),
        usage_pct: usage_pct.clamp(0.0, 100.0),
        temp_c,
        vram_used_mib,
        vram_total_mib,
    })
}

fn detect_amd_gpu_name(card_path: &Path, device_path: &Path, card_name: &str) -> String {
    if let Some(name) = read_trimmed_file(&device_path.join("product_name")) {
        if !name.is_empty() {
            return with_amd_prefix(&name);
        }
    }

    let mut pci_slot_name = None;
    if let Some(uevent_text) = read_trimmed_file(&device_path.join("uevent")) {
        if let Some(pci_slot) = parse_uevent_field(&uevent_text, "PCI_SLOT_NAME") {
            if let Some(name) = read_pci_name_with_lspci(&pci_slot) {
                return name;
            }
            pci_slot_name = Some(pci_slot);
        }
    }

    if let (Some(vendor_id), Some(device_id)) = (
        read_pci_hex_id(&device_path.join("vendor")),
        read_pci_hex_id(&device_path.join("device")),
    ) {
        if let Some(name) = read_pci_name_from_ids(&vendor_id, &device_id) {
            return with_amd_prefix(&name);
        }
    }

    if let Some(pci_slot) = pci_slot_name {
        return format!("AMD {pci_slot}");
    }

    if let Some(label) = card_path.file_name().and_then(|label| label.to_str()) {
        return format!("AMD {label}");
    }

    format!("AMD {card_name}")
}

fn detect_intel_gpu_name(card_path: &Path, device_path: &Path, card_name: &str) -> String {
    if let Some(name) = read_trimmed_file(&device_path.join("product_name")) {
        if !name.is_empty() {
            return with_intel_prefix(&name);
        }
    }

    let mut pci_slot_name = None;
    if let Some(uevent_text) = read_trimmed_file(&device_path.join("uevent")) {
        if let Some(pci_slot) = parse_uevent_field(&uevent_text, "PCI_SLOT_NAME") {
            if let Some(name) = read_pci_name_with_lspci_raw(&pci_slot) {
                return with_intel_prefix(&name);
            }
            pci_slot_name = Some(pci_slot);
        }
    }

    if let (Some(vendor_id), Some(device_id)) = (
        read_pci_hex_id(&device_path.join("vendor")),
        read_pci_hex_id(&device_path.join("device")),
    ) {
        if let Some(name) = read_pci_name_from_ids(&vendor_id, &device_id) {
            return with_intel_prefix(&name);
        }
    }

    if let Some(pci_slot) = pci_slot_name {
        return format!("Intel {pci_slot}");
    }

    if let Some(label) = card_path.file_name().and_then(|label| label.to_str()) {
        return format!("Intel {label}");
    }

    format!("Intel {card_name}")
}

fn parse_uevent_field(uevent_text: &str, key: &str) -> Option<String> {
    for line in uevent_text.lines() {
        let Some((field, value)) = line.split_once('=') else {
            continue;
        };
        if field == key {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn read_pci_name_with_lspci(pci_slot: &str) -> Option<String> {
    let device_desc = read_pci_name_with_lspci_raw(pci_slot)?;
    if let Some(radeon_name) = extract_bracketed_name(&device_desc, "Radeon") {
        return Some(format!("AMD {radeon_name}"));
    }
    Some(with_amd_prefix(&device_desc))
}

fn read_pci_name_with_lspci_raw(pci_slot: &str) -> Option<String> {
    let output = Command::new("lspci").args(["-s", pci_slot]).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    let line = text.lines().next()?.trim();
    let (_, device_desc) = line.split_once(": ")?;
    let device_desc = device_desc
        .split(" (rev ")
        .next()
        .unwrap_or(device_desc)
        .trim();
    if device_desc.is_empty() {
        return None;
    }

    Some(device_desc.to_string())
}

fn read_pci_name_from_ids(vendor_id: &str, device_id: &str) -> Option<String> {
    for path in ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids"] {
        if let Some(name) = read_pci_name_from_ids_file(Path::new(path), vendor_id, device_id) {
            return Some(name);
        }
    }
    None
}

fn read_pci_name_from_ids_file(path: &Path, vendor_id: &str, device_id: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let mut in_vendor_block = false;

    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if !line.starts_with('\t') {
            let mut parts = line.split_whitespace();
            let Some(current_vendor) = parts.next() else {
                continue;
            };
            in_vendor_block = current_vendor.eq_ignore_ascii_case(vendor_id);
            continue;
        }

        if !in_vendor_block {
            continue;
        }

        if !line.starts_with('\t') || line.starts_with("\t\t") {
            continue;
        }

        let trimmed = line.trim_start_matches('\t');
        let mut parts = trimmed.split_whitespace();
        let Some(current_device) = parts.next() else {
            continue;
        };
        if !current_device.eq_ignore_ascii_case(device_id) {
            continue;
        }

        let name = trimmed.get(current_device.len()..)?.trim();
        if name.is_empty() {
            continue;
        }
        return Some(name.to_string());
    }

    None
}

fn read_pci_hex_id(path: &Path) -> Option<String> {
    let raw = read_trimmed_file(path)?;
    let id = raw.strip_prefix("0x").unwrap_or(&raw).trim();
    if id.is_empty() {
        return None;
    }
    Some(id.to_ascii_lowercase())
}

fn with_amd_prefix(name: &str) -> String {
    let normalized = collapse_whitespace(name);
    let normalized_lower = normalized.to_ascii_lowercase();
    if normalized_lower.starts_with("amd ")
        || normalized_lower.starts_with("advanced micro devices")
    {
        normalized
    } else {
        format!("AMD {normalized}")
    }
}

fn with_intel_prefix(name: &str) -> String {
    let normalized = collapse_whitespace(name);
    let normalized_lower = normalized.to_ascii_lowercase();
    if normalized_lower.starts_with("intel ") || normalized_lower.starts_with("intel corporation") {
        normalized
    } else {
        format!("Intel {normalized}")
    }
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_bracketed_name(input: &str, starts_with: &str) -> Option<String> {
    let needle = format!("[{starts_with}");
    let start = input.find(&needle)?;
    let from_bracket = &input[start + 1..];
    let end = from_bracket.find(']')?;
    let bracketed = from_bracket[..end].trim();
    if bracketed.is_empty() {
        return None;
    }
    Some(bracketed.to_string())
}

fn read_hwmon_temp_c(device_path: &Path) -> Option<f32> {
    let hwmon_root = device_path.join("hwmon");
    let hwmon_entries = fs::read_dir(hwmon_root).ok()?;
    for entry in hwmon_entries.flatten() {
        let sensor_dir = entry.path();
        for temp_file in ["temp1_input", "temp2_input", "temp3_input"] {
            let Some(temp_milli_c) = read_u64_from_file(&sensor_dir.join(temp_file)) else {
                continue;
            };
            return Some(temp_milli_c as f32 / 1000.0);
        }
    }
    None
}

fn read_intel_usage_pct(card_path: &Path, device_path: &Path) -> Option<f32> {
    for path in [
        card_path.join("gt_busy_percent"),
        device_path.join("gpu_busy_percent"),
        device_path.join("gt_busy_percent"),
        card_path.join("gt/gt0/busy"),
        device_path.join("gt/gt0/busy"),
        device_path.join("tile0/gt0/busy"),
    ] {
        if let Some(usage_pct) = read_f32_from_file(&path) {
            return Some(usage_pct.clamp(0.0, 100.0));
        }
    }

    for (act_path, max_path) in [
        (
            card_path.join("gt_act_freq_mhz"),
            card_path.join("gt_max_freq_mhz"),
        ),
        (
            device_path.join("gt/gt0/rps_act_freq_mhz"),
            device_path.join("gt/gt0/rps_max_freq_mhz"),
        ),
        (
            device_path.join("tile0/gt0/freq0/act_freq"),
            device_path.join("tile0/gt0/freq0/max_freq"),
        ),
        (
            device_path.join("gt/gt0/freq0/act_freq"),
            device_path.join("gt/gt0/freq0/max_freq"),
        ),
    ] {
        let Some(act_freq) = read_f32_from_file(&act_path) else {
            continue;
        };
        let Some(max_freq) = read_f32_from_file(&max_path) else {
            continue;
        };
        if max_freq > 0.0 {
            return Some((act_freq / max_freq * 100.0).clamp(0.0, 100.0));
        }
    }

    None
}

fn read_intel_local_memory_mib(device_path: &Path) -> Option<(f32, f32)> {
    if let (Some(used_bytes), Some(total_bytes)) = (
        read_u64_from_file(&device_path.join("lmem_used_bytes")),
        read_u64_from_file(&device_path.join("lmem_total_bytes")),
    ) {
        return Some((bytes_to_mib(used_bytes), bytes_to_mib(total_bytes)));
    }

    read_intel_memory_regions_mib(device_path)
}

fn read_intel_memory_regions_mib(device_path: &Path) -> Option<(f32, f32)> {
    let entries = fs::read_dir(device_path).ok()?;
    let mut total_bytes = 0_u64;
    let mut used_bytes = 0_u64;

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("memory_region") {
            continue;
        }

        let is_local = read_trimmed_file(&path.join("name"))
            .map(|region_name| {
                let region_name = region_name.to_ascii_lowercase();
                region_name.starts_with("local") || region_name.starts_with("lmem")
            })
            .unwrap_or(false);
        if !is_local {
            continue;
        }

        let Some(region_total) = read_u64_from_file(&path.join("total"))
            .or_else(|| read_u64_from_file(&path.join("size")))
        else {
            continue;
        };
        let region_used = read_u64_from_file(&path.join("used")).unwrap_or(0);
        total_bytes = total_bytes.saturating_add(region_total);
        used_bytes = used_bytes.saturating_add(region_used.min(region_total));
    }

    if total_bytes == 0 {
        return None;
    }

    Some((bytes_to_mib(used_bytes), bytes_to_mib(total_bytes)))
}

fn read_trimmed_file(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

fn read_u64_from_file(path: &Path) -> Option<u64> {
    read_trimmed_file(path)?.parse::<u64>().ok()
}

fn read_f32_from_file(path: &Path) -> Option<f32> {
    read_trimmed_file(path)?.parse::<f32>().ok()
}

pub fn read_root_storage() -> Option<StorageStats> {
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

pub fn parse_logical_cpu_id(name: &str, fallback: usize) -> usize {
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

pub fn pair_entries_count(pairs: &[(usize, Option<usize>)]) -> usize {
    pairs
        .iter()
        .map(|(_, right)| 1 + usize::from(right.is_some()))
        .sum()
}

pub fn detect_cpu_pairs(ids: Vec<usize>) -> Vec<(usize, Option<usize>)> {
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

pub fn detect_cpu_model(system: &System) -> String {
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

fn bytes_to_mib(bytes: u64) -> f32 {
    bytes as f32 / (1024.0 * 1024.0)
}
