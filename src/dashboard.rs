//! Live full-screen dashboard fed by stats.json.

use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table};
use serde_json::Value;

use crate::paths;

fn load() -> Value {
    fs::read_to_string(paths::stats_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
}

pub fn human_bytes(n: f64) -> String {
    let mut n = n;
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if n < 1024.0 {
            return format!("{n:.1}{unit}");
        }
        n /= 1024.0;
    }
    format!("{n:.1}PB")
}

pub fn human_time(secs: i64) -> String {
    let (m, s) = (secs.max(0) / 60, secs.max(0) % 60);
    let (h, m) = (m / 60, m % 60);
    if h > 0 { format!("{h}h{m:02}m") } else { format!("{m}m{s:02}s") }
}

fn ago(t: f64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let d = (now - t).max(0.0);
    if d < 60.0 { format!("{}s ago", d as i64) } else { format!("{}m ago", (d / 60.0) as i64) }
}

fn state(s: &Value) -> (Color, &'static str) {
    if !s["running"].as_bool().unwrap_or(false) {
        (Color::Red, "stopped")
    } else if s["error"].as_bool().unwrap_or(false) {
        (Color::Red, "error")
    } else if s["idle"].as_bool().unwrap_or(false) {
        (Color::Yellow, "idle")
    } else {
        (Color::Green, "running")
    }
}

fn panel(title: &str) -> Block<'_> {
    Block::bordered().title(title).border_style(Style::new().fg(Color::DarkGray))
}

fn kv<'a>(key: &'a str, value: String, color: Color) -> Line<'a> {
    Line::from(vec![Span::raw(key).dim(), Span::styled(value, Style::new().fg(color))])
}

fn draw(f: &mut Frame, s: &Value) {
    let [top, mid, bottom, foot] =
        Layout::vertical([Constraint::Length(8), Constraint::Length(10), Constraint::Min(3), Constraint::Length(2)])
            .areas(f.area());
    let [status_area, totals_area] = Layout::horizontal([Constraint::Ratio(1, 2); 2]).areas(top);

    let (color, label) = state(s);
    let status = vec![
        Line::from(vec![
            Span::styled("●  ", Style::new().fg(color)),
            Span::styled(label, Style::new().fg(color).bold()),
        ]),
        Line::raw(""),
        kv("mode    ", s["mode"].as_str().unwrap_or("?").to_string(), Color::Reset),
        kv("uptime  ", human_time(s["uptime"].as_i64().unwrap_or(0)), Color::Reset),
        kv("errors  ", s["error_count"].as_i64().unwrap_or(0).to_string(), Color::Reset),
    ];
    f.render_widget(Paragraph::new(status).block(panel("status")), status_area);

    let totals = vec![
        kv("articles  ", s["total_articles"].as_i64().unwrap_or(0).to_string(), Color::Cyan),
        kv("releases  ", s["total_releases"].as_i64().unwrap_or(0).to_string(), Color::Cyan),
        kv("indexed   ", human_bytes(s["total_bytes"].as_f64().unwrap_or(0.0)), Color::Cyan),
        kv("db size   ", human_bytes(s["db_size"].as_f64().unwrap_or(0.0)), Color::Cyan),
    ];
    f.render_widget(Paragraph::new(totals).block(panel("totals")), totals_area);

    draw_speed(f, s, mid);
    draw_groups(f, s, bottom);

    f.render_widget(Paragraph::new(Line::raw("  Ctrl+C / q to go back").dim()), foot);
}

fn draw_speed(f: &mut Frame, s: &Value, area: Rect) {
    // bytes indexed per second between consecutive history points
    let points: Vec<(f64, f64)> = s["history"]
        .as_array()
        .map(|h| h.iter().map(|x| (x["t"].as_f64().unwrap_or(0.0), x["b"].as_f64().unwrap_or(0.0))).collect())
        .unwrap_or_default();

    let mut rate: Vec<(f64, f64)> = points
        .windows(2)
        .enumerate()
        .map(|(i, w)| {
            let dt = w[1].0 - w[0].0;
            (i as f64, if dt > 0.0 { w[1].1 / dt } else { 0.0 })
        })
        .collect();
    if rate.len() < 2 {
        rate = vec![(0.0, 0.0), (1.0, 0.0)];
    }

    let max_y = rate.iter().map(|p| p.1).fold(1.0, f64::max);
    let max_x = (rate.len() - 1) as f64;

    let block = panel("throughput").title_bottom(
        Line::from(format!(
            " avg {}/s   peak {}/s ",
            human_bytes(s["avg_byte_speed"].as_f64().unwrap_or(0.0)),
            human_bytes(s["peak_byte_speed"].as_f64().unwrap_or(0.0))
        ))
        .dim(),
    );

    let dataset = Dataset::default().marker(Marker::Braille).graph_type(GraphType::Line).cyan().data(&rate);
    let chart = Chart::new(vec![dataset]).block(block).x_axis(Axis::default().bounds([0.0, max_x.max(1.0)])).y_axis(
        Axis::default()
            .bounds([0.0, max_y])
            .labels(vec![Span::raw("0").dim(), Span::raw(format!("{}/s", human_bytes(max_y))).dim()]),
    );

    f.render_widget(chart, area);
}

fn bar(value: f64, max: f64, width: usize) -> String {
    let filled = if max > 0.0 { ((value / max) * width as f64).round() as usize } else { 0 };
    format!("{}{}", "━".repeat(filled.min(width)), " ".repeat(width.saturating_sub(filled)))
}

fn draw_groups(f: &mut Frame, s: &Value, area: Rect) {
    let empty = serde_json::Map::new();
    let groups = s["groups"].as_object().unwrap_or(&empty);
    let max_art = groups.values().map(|g| g["articles"].as_f64().unwrap_or(0.0)).fold(1.0, f64::max);
    let bar_width = (area.width as usize).saturating_sub(60).clamp(10, 60);

    let rows: Vec<Row> = groups
        .iter()
        .map(|(name, g)| {
            let articles = g["articles"].as_f64().unwrap_or(0.0);
            Row::new(vec![
                Cell::from(name.as_str()).cyan(),
                Cell::from(format!("{}", articles as i64)),
                Cell::from(bar(articles, max_art, bar_width)).magenta(),
                Cell::from(ago(g["last_indexed"].as_f64().unwrap_or(0.0))).dim(),
            ])
        })
        .collect();

    let header =
        Row::new(vec!["group", "articles", "load", "last indexed"]).style(Style::new().add_modifier(Modifier::BOLD));
    let title = format!("groups ({})", s["groups_indexed"].as_i64().unwrap_or(0));
    let table = Table::new(
        rows,
        [Constraint::Fill(2), Constraint::Length(10), Constraint::Length(bar_width as u16), Constraint::Length(14)],
    )
    .header(header)
    .block(panel(&title));

    f.render_widget(table, area);
}

/// Run until ctrl+c / q / esc.
pub fn run() -> std::io::Result<()> {
    let mut terminal = ratatui::init();

    let result = (|| -> std::io::Result<()> {
        loop {
            let stats = load();
            terminal.draw(|f| draw(f, &stats))?;

            if event::poll(Duration::from_millis(500))?
                && let Event::Key(key) = event::read()?
            {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl_c = key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
                if ctrl_c || matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    return Ok(());
                }
            }
        }
    })();

    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(human_bytes(512.0), "512.0B");
        assert_eq!(human_bytes(2048.0), "2.0KB");
        assert_eq!(human_time(59), "0m59s");
        assert_eq!(human_time(3720), "1h02m");
        assert_eq!(bar(5.0, 10.0, 4), "━━  ");
    }

    #[test]
    fn renders_without_stats() {
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, &Value::Null)).unwrap();

        let stats = serde_json::json!({
            "running": true, "idle": false, "mode": "dynamic", "uptime": 65,
            "history": [{"t": 1.0, "a": 1, "b": 100}, {"t": 2.0, "a": 1, "b": 300}],
            "groups": {"alt.binaries.test": {"articles": 10, "releases": 2, "last_indexed": 0.0}},
            "groups_indexed": 1
        });
        term.draw(|f| draw(f, &stats)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("alt.binaries.test"));
        assert!(text.contains("running"));

        let fast = serde_json::json!({"avg_byte_speed": 1382195856.6, "peak_byte_speed": 4682519700.9});
        term.draw(|f| draw(f, &fast)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("avg 1.3GB/s   peak 4.4GB/s"), "{text}");
    }
}
