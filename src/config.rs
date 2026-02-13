use std::time::Duration;

pub const DRAW_INTERVAL: Duration = Duration::from_millis(240);
pub const CPU_INTERVAL: Duration = Duration::from_millis(800);
pub const PROCESS_INTERVAL: Duration = Duration::from_millis(800);
pub const GPU_INTERVAL: Duration = Duration::from_millis(800);
pub const STORAGE_INTERVAL: Duration = Duration::from_secs(4);
pub const PROCESS_FREEZE_AFTER_NAV: Duration = Duration::from_secs(2);
pub const PROCESS_GROUP_DISAPPEAR_GRACE: Duration = Duration::from_secs(3);
pub const STATUS_MESSAGE_DURATION: Duration = Duration::from_secs(3);

pub const MAX_CONTENT_WIDTH: u16 = 160;
pub const CPU_LAYOUT_SLACK_ROWS: u16 = 0;
