use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::{Color, Line, Modifier, Span, Style};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table};
use std::time::Instant;

use crate::app::{App, KillMenuTarget, ProcessListRow};
use crate::config::{CPU_LAYOUT_SLACK_ROWS, MAX_CONTENT_WIDTH};
use crate::models::GpuStats;
use crate::theme::{
    COLOR_ACCENT_CPU, COLOR_ACCENT_GPU, COLOR_ACCENT_PROC, COLOR_ACCENT_THREAD, COLOR_ACCENT_VRAM,
    COLOR_BG, COLOR_BORDER, COLOR_HEADER_BG, COLOR_HOT, COLOR_MUTED, COLOR_OK, COLOR_ROW_A,
    COLOR_ROW_B, COLOR_ROW_SELECTED, COLOR_SEPARATOR, COLOR_TEXT, COLOR_TRACK, COLOR_WARN,
    style_for_temp, style_for_usage_with_base,
};

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

fn format_disk_usage(used_bytes: u64, total_bytes: u64) -> String {
    const SMALL_DISK_THRESHOLD_T: f64 = 0.6;
    let total_t = bytes_to_t(total_bytes);
    if total_t < SMALL_DISK_THRESHOLD_T {
        let used_gb = bytes_to_gb(used_bytes);
        let total_gb = bytes_to_gb(total_bytes);
        format!("{used_gb:.1}/{total_gb:.1} GB")
    } else {
        let used_t = bytes_to_t(used_bytes);
        format!("{used_t:.2}/{total_t:.2} T")
    }
}

fn mib_to_gb(mib: f32) -> f32 {
    mib / 1024.0
}

pub fn draw_ui(f: &mut Frame<'_>, app: &App, now: Instant) {
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

    let cpu_rows_needed = if app.combine_smt {
        app.cpu_pairs.len().div_ceil(2)
    } else {
        app.cpu_pairs.len()
    };
    let cpu_target = cpu_rows_needed
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
    render_gpu_panel(f, middle[0], app.gpu.as_ref());
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
    let title_text = truncate_ascii(&app.cpu_model, title_width.saturating_sub(10));
    let freq_text = app
        .cpu_freq_mhz
        .map(|mhz| format!("{:.1} GHz", mhz as f64 / 1000.0))
        .unwrap_or_else(|| "n/a".to_string());

    let title_line = Line::from(vec![
        Span::styled(
            format!(" {title_text} "),
            Style::default()
                .fg(COLOR_ACCENT_CPU)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            freq_text,
            Style::default()
                .fg(COLOR_MUTED)
                .add_modifier(Modifier::BOLD),
        ),
    ])
    .centered();

    let block = Block::default()
        .title(title_line)
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
    let shown = if app.combine_smt {
        (rows * 2).min(app.cpu_pairs.len())
    } else {
        rows.min(app.cpu_pairs.len())
    };
    let (summary_label, summary_count) = if app.combine_smt {
        ("Combined ", app.cpu_pairs.len())
    } else {
        ("Cores ", app.cpu_pairs.len())
    };
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
        Span::styled(" | ", Style::default().fg(COLOR_MUTED)),
        Span::styled(summary_label, Style::default().fg(COLOR_MUTED)),
        Span::styled(
            format!("{}", summary_count),
            Style::default()
                .fg(COLOR_ACCENT_CPU)
                .add_modifier(Modifier::BOLD),
        ),
    ]))
    .style(Style::default().bg(COLOR_ROW_A));
    f.render_widget(summary, chunks[0]);

    if app.combine_smt {
        let pairs_per_row = 2;
        let num_rows = shown.div_ceil(pairs_per_row);
        let actual_rows = num_rows.min(rows);
        let mut row_constraints = vec![Constraint::Length(1); actual_rows];
        row_constraints.push(Constraint::Min(0));
        let row_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(row_constraints)
            .split(chunks[1]);

        for (row_idx, row_area) in row_chunks.iter().take(actual_rows).enumerate() {
            let left_idx = row_idx * pairs_per_row;
            let right_idx = left_idx + 1;

            if row_area.width >= 52 {
                let col_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Percentage(50),
                        Constraint::Length(1),
                        Constraint::Percentage(49),
                    ])
                    .split(*row_area);

                let row_bg = if row_idx.is_multiple_of(2) {
                    COLOR_ROW_A
                } else {
                    COLOR_ROW_B
                };

                if let Some((core_id, smt_id)) = app.cpu_pairs.get(left_idx) {
                    let core_usage = app.cpu_usage_by_id.get(core_id).copied().unwrap_or(0.0);
                    let combined_usage = if let Some(smt_id) = smt_id {
                        let smt_usage = app.cpu_usage_by_id.get(smt_id).copied().unwrap_or(0.0);
                        (core_usage + smt_usage) / 2.0
                    } else {
                        core_usage
                    };
                    let line = cpu_lane_line(
                        "c",
                        *core_id,
                        combined_usage,
                        col_chunks[0].width as usize,
                        COLOR_ACCENT_CPU,
                    );
                    f.render_widget(
                        Paragraph::new(line).style(Style::default().bg(row_bg)),
                        col_chunks[0],
                    );
                }

                let divider =
                    Paragraph::new(Line::from("│").style(Style::default().fg(COLOR_SEPARATOR)))
                        .style(Style::default().bg(row_bg));
                f.render_widget(divider, col_chunks[1]);

                if let Some((core_id, smt_id)) = app.cpu_pairs.get(right_idx) {
                    let core_usage = app.cpu_usage_by_id.get(core_id).copied().unwrap_or(0.0);
                    let combined_usage = if let Some(smt_id) = smt_id {
                        let smt_usage = app.cpu_usage_by_id.get(smt_id).copied().unwrap_or(0.0);
                        (core_usage + smt_usage) / 2.0
                    } else {
                        core_usage
                    };
                    let line = cpu_lane_line(
                        "c",
                        *core_id,
                        combined_usage,
                        col_chunks[2].width as usize,
                        COLOR_ACCENT_CPU,
                    );
                    f.render_widget(
                        Paragraph::new(line).style(Style::default().bg(row_bg)),
                        col_chunks[2],
                    );
                }
            }
        }
    } else {
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
}

fn render_gpu_panel(f: &mut Frame<'_>, area: Rect, gpu: Option<&GpuStats>) {
    let title_width = area.width.saturating_sub(4) as usize;
    let title_text = if let Some(stats) = gpu {
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

    let Some(stats) = gpu else {
        let note = Paragraph::new("No GPU metrics (NVIDIA nvidia-smi / AMD amdgpu sysfs)")
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

    let (disk_text, disk_pct) = if let Some(storage) = &app.storage {
        let disk_pct = if storage.total_bytes == 0 {
            0.0
        } else {
            (storage.used_bytes as f32 / storage.total_bytes as f32 * 100.0).clamp(0.0, 100.0)
        };
        (
            format_disk_usage(storage.used_bytes, storage.total_bytes),
            disk_pct,
        )
    } else {
        ("n/a".to_string(), 0.0)
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

        let disk_line = value_usage_line(&disk_text, disk_pct, COLOR_ACCENT_VRAM);
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
            Span::styled(disk_text, Style::default().fg(COLOR_ACCENT_VRAM)),
            Span::styled(
                format!(" {disk_pct:.1}%"),
                style_for_usage_with_base(disk_pct, COLOR_ACCENT_VRAM),
            ),
        ]);
        f.render_widget(Paragraph::new(line_bottom), chunks[1]);
    }
}

fn render_process_table(f: &mut Frame<'_>, area: Rect, app: &App, now: Instant) {
    let visible_rows = app.visible_process_rows();
    let selected_row = app.selected_row_index();

    let row_capacity = area.height.saturating_sub(3) as usize;
    let (row_start, row_end) = if row_capacity == 0 || visible_rows.is_empty() {
        (0, 0)
    } else if let Some(selected) = selected_row {
        let max_start = visible_rows.len().saturating_sub(row_capacity);
        let start = selected.saturating_sub(row_capacity / 2).min(max_start);
        let end = (start + row_capacity).min(visible_rows.len());
        (start, end)
    } else {
        (0, row_capacity.min(visible_rows.len()))
    };

    let mut rows = Vec::with_capacity(row_end.saturating_sub(row_start));

    for (offset, row) in visible_rows[row_start..row_end].iter().enumerate() {
        let row_idx = row_start + offset;
        let is_selected = selected_row.is_some_and(|selected| selected == row_idx);
        let row_bg = if is_selected {
            COLOR_ROW_SELECTED
        } else if row_idx.is_multiple_of(2) {
            COLOR_ROW_A
        } else {
            COLOR_ROW_B
        };

        match row {
            ProcessListRow::App { app_index } => {
                let Some(group) = app.process_group_at(*app_index) else {
                    continue;
                };
                let marker = if group.stale {
                    "•"
                } else if app.is_expanded_app_index(*app_index) {
                    "▾"
                } else {
                    "▸"
                };
                let app_label = format!("{marker} {} ({})", group.app, group.processes.len());

                rows.push(Row::new(vec![
                    Cell::from(app_label).style(
                        Style::default()
                            .fg(if is_selected {
                                COLOR_TEXT
                            } else if group.stale {
                                COLOR_MUTED
                            } else {
                                COLOR_ACCENT_PROC
                            })
                            .bg(row_bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Cell::from("—").style(Style::default().fg(COLOR_MUTED).bg(row_bg)),
                    Cell::from(format!("{} MiB", group.total_mem_mib)).style(
                        Style::default()
                            .fg(COLOR_ACCENT_VRAM)
                            .bg(row_bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Cell::from(format!("{:.1}%", group.total_cpu)).style(
                        style_for_usage_with_base(group.total_cpu, COLOR_ACCENT_THREAD)
                            .bg(row_bg)
                            .add_modifier(if is_selected {
                                Modifier::BOLD | Modifier::UNDERLINED
                            } else {
                                Modifier::BOLD
                            }),
                    ),
                ]));
            }
            ProcessListRow::Process {
                app_index,
                process_index,
            } => {
                let Some(process) = app.process_at(*app_index, *process_index) else {
                    continue;
                };
                rows.push(Row::new(vec![
                    Cell::from(format!("  └─ {}", process.name)).style(
                        Style::default()
                            .fg(COLOR_TEXT)
                            .bg(row_bg)
                            .add_modifier(if is_selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ),
                    Cell::from(process.pid.clone()).style(
                        Style::default()
                            .fg(if is_selected {
                                COLOR_TEXT
                            } else {
                                COLOR_ACCENT_PROC
                            })
                            .bg(row_bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Cell::from(format!("{} MiB", process.mem_mib)).style(
                        Style::default()
                            .fg(COLOR_ACCENT_VRAM)
                            .bg(row_bg)
                            .add_modifier(if is_selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ),
                    Cell::from(format!("{:.1}%", process.cpu)).style(
                        style_for_usage_with_base(process.cpu, COLOR_ACCENT_THREAD)
                            .bg(row_bg)
                            .add_modifier(if is_selected {
                                Modifier::BOLD | Modifier::UNDERLINED
                            } else {
                                Modifier::BOLD
                            }),
                    ),
                ]));
            }
        }
    }

    let title = if app.expanded_app_name().is_some() {
        " Processes (expanded, frozen) "
    } else if app.process_updates_locked(now) {
        " Processes (frozen) "
    } else {
        " Processes "
    };

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(60),
            Constraint::Length(8),
            Constraint::Length(14),
            Constraint::Length(10),
        ],
    )
    .header(
        Row::new(vec!["App", "PID", "Memory", "CPU"]).style(
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
    let Some(target) = app.kill_menu_target() else {
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
    let lines = match target {
        KillMenuTarget::Process { pid, name } => {
            let name = truncate_ascii(name, title_width.max(1));
            vec![
                Line::from(vec![
                    Span::styled("PID ", Style::default().fg(COLOR_MUTED)),
                    Span::styled(
                        pid.clone(),
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
                    Span::styled(" SIGTERM process   ", Style::default().fg(COLOR_MUTED)),
                    Span::styled(
                        "f",
                        Style::default().fg(COLOR_WARN).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" SIGKILL process", Style::default().fg(COLOR_MUTED)),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Esc",
                        Style::default()
                            .fg(COLOR_ACCENT_PROC)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" cancel", Style::default().fg(COLOR_MUTED)),
                ]),
            ]
        }
        KillMenuTarget::App {
            app, process_count, ..
        } => {
            let app_name = truncate_ascii(app, title_width.max(1));
            vec![
                Line::from(vec![
                    Span::styled("App ", Style::default().fg(COLOR_MUTED)),
                    Span::styled(
                        app_name,
                        Style::default()
                            .fg(COLOR_ACCENT_PROC)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("  ", Style::default()),
                    Span::styled("Processes ", Style::default().fg(COLOR_MUTED)),
                    Span::styled(
                        process_count.to_string(),
                        Style::default().fg(COLOR_TEXT).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled(
                        "Enter / k",
                        Style::default().fg(COLOR_OK).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" SIGTERM app   ", Style::default().fg(COLOR_MUTED)),
                    Span::styled(
                        "f",
                        Style::default().fg(COLOR_WARN).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" SIGKILL app", Style::default().fg(COLOR_MUTED)),
                ]),
                Line::from(vec![
                    Span::styled(
                        "o / →",
                        Style::default()
                            .fg(COLOR_ACCENT_PROC)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" open process list   ", Style::default().fg(COLOR_MUTED)),
                    Span::styled(
                        "Esc",
                        Style::default()
                            .fg(COLOR_ACCENT_PROC)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" cancel", Style::default().fg(COLOR_MUTED)),
                ]),
            ]
        }
    };
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
            "←/→",
            Style::default()
                .fg(COLOR_ACCENT_PROC)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" close/open app  |  ", Style::default().fg(COLOR_MUTED)),
        Span::styled(
            "Enter",
            Style::default()
                .fg(COLOR_ACCENT_PROC)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" actions  |  ", Style::default().fg(COLOR_MUTED)),
        Span::styled(
            "m",
            Style::default()
                .fg(if app.combine_smt {
                    COLOR_OK
                } else {
                    COLOR_ACCENT_PROC
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if app.combine_smt {
                " cpu mode  |  "
            } else {
                " thread mode  |  "
            },
            Style::default().fg(COLOR_MUTED),
        ),
        Span::styled(
            "q/Esc",
            Style::default()
                .fg(COLOR_ACCENT_PROC)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" quit", Style::default().fg(COLOR_MUTED)),
    ];

    if let Some(expanded_app) = app.expanded_app_name() {
        spans.push(Span::styled("  |  ", Style::default().fg(COLOR_MUTED)));
        spans.push(Span::styled(
            format!("expanded {expanded_app} (refresh paused)"),
            Style::default().fg(COLOR_OK).add_modifier(Modifier::BOLD),
        ));
    } else if let Some(remaining) = app.process_freeze_remaining(now) {
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
