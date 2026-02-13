mod app;
mod collectors;
mod config;
mod models;
mod theme;
mod tui;
mod ui;

use anyhow::Result;

fn main() -> Result<()> {
    tui::run_tui()
}
