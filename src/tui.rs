use std::io::{self, Stdout};
use std::time::Instant;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;

use crate::app::App;
use crate::config::{
    CPU_INTERVAL, DRAW_INTERVAL, GPU_INTERVAL, PROCESS_INTERVAL, STORAGE_INTERVAL,
};
use crate::models::KillSignal;
use crate::ui::draw_ui;

pub fn run_tui() -> Result<()> {
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

        let process_due = if app.process_updates_locked(now) {
            now + PROCESS_INTERVAL
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
                            app.close_kill_menu();
                            redraw_now = true;
                        }
                        KeyCode::Enter | KeyCode::Char('k') => {
                            redraw_now = true;
                            let result = app.send_kill_to_selected(KillSignal::Term);
                            app.close_kill_menu();
                            match result {
                                Ok(message) => {
                                    app.set_status(now, message, false);
                                    app.refresh_processes();
                                    last_process = now;
                                }
                                Err(err) => {
                                    app.set_status(now, err.to_string(), true);
                                }
                            }
                        }
                        KeyCode::Char('f') | KeyCode::Char('K') => {
                            redraw_now = true;
                            let result = app.send_kill_to_selected(KillSignal::Kill);
                            app.close_kill_menu();
                            match result {
                                Ok(message) => {
                                    app.set_status(now, message, false);
                                    app.refresh_processes();
                                    last_process = now;
                                }
                                Err(err) => {
                                    app.set_status(now, err.to_string(), true);
                                }
                            }
                        }
                        KeyCode::Right | KeyCode::Char('o') => {
                            if app.expand_kill_menu_app_target() {
                                app.close_kill_menu();
                                redraw_now = true;
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
                    KeyCode::Char('m') => {
                        app.combine_smt = !app.combine_smt;
                        redraw_now = true;
                    }
                    KeyCode::Up => {
                        app.move_selection(-1, now);
                        redraw_now = true;
                    }
                    KeyCode::Down => {
                        app.move_selection(1, now);
                        redraw_now = true;
                    }
                    KeyCode::Right => {
                        redraw_now = app.expand_selected_app();
                    }
                    KeyCode::Left => {
                        redraw_now = app.collapse_expanded_app();
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
