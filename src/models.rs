use std::time::Instant;

#[derive(Debug, Clone)]
pub struct ProcessStat {
    pub app: String,
    pub pid: String,
    pub name: String,
    pub mem_mib: u64,
    pub cpu: f32,
}

#[derive(Debug, Clone)]
pub struct ProcessGroup {
    pub app: String,
    pub total_mem_mib: u64,
    pub total_cpu: f32,
    pub stale: bool,
    pub processes: Vec<ProcessStat>,
}

#[derive(Debug, Clone)]
pub struct GpuStats {
    pub name: String,
    pub usage_pct: f32,
    pub temp_c: f32,
    pub vram_used_mib: f32,
    pub vram_total_mib: f32,
}

#[derive(Debug, Clone)]
pub struct StorageStats {
    pub used_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum KillSignal {
    Term,
    Kill,
}

impl KillSignal {
    pub fn as_kill_arg(self) -> &'static str {
        match self {
            Self::Term => "-TERM",
            Self::Kill => "-KILL",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    pub is_error: bool,
    pub expires_at: Instant,
}
