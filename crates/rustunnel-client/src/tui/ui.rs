//! Terminal UI rendering.
//!
//! Layout, top to bottom: session header, tunnel list, traffic counters, the
//! live request log (or the log pane), and a key hint footer. Colours follow
//! the palette the client already used for request lines.

use chrono::Utc;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Sparkline, Table};
use ratatui::Frame;

use super::App;
use crate::inspect::{format_bytes, Exchange, Session, SessionStatus};

/// Rows reserved for the tunnel list before it starts scrolling.
const MAX_TUNNEL_ROWS: usize = 4;

pub fn draw(frame: &mut Frame, app: &App) {
    let session = app.inspector.session();
    let tunnel_rows = session.tunnels.len().clamp(1, MAX_TUNNEL_ROWS) as u16;

    let areas = Layout::vertical([
        Constraint::Length(5),               // header
        Constraint::Length(tunnel_rows + 2), // tunnels
        Constraint::Length(4),               // traffic
        Constraint::Min(5),                  // requests / logs
        Constraint::Length(1),               // footer
    ])
    .split(frame.area());

    draw_header(frame, areas[0], &session);
    draw_tunnels(frame, areas[1], &session);
    draw_traffic(frame, areas[2], app);
    if app.show_logs {
        draw_logs(frame, areas[3], app);
    } else {
        draw_requests(frame, areas[3], app);
    }
    draw_footer(frame, areas[4], app);
}

// ── header ────────────────────────────────────────────────────────────────────

fn draw_header(frame: &mut Frame, area: Rect, session: &Session) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " rustunnel ",
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Left);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let columns =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(inner);

    let (status_symbol, status_style) = match &session.status {
        SessionStatus::Online => ("●", Style::default().fg(Color::Green)),
        SessionStatus::Connecting => ("◌", Style::default().fg(Color::Yellow)),
        SessionStatus::Reconnecting { .. } => ("◌", Style::default().fg(Color::Yellow)),
        SessionStatus::Closed => ("●", Style::default().fg(Color::Red)),
    };

    let left = vec![
        Line::from(vec![
            label("Session"),
            Span::styled(status_symbol, status_style),
            Span::raw(" "),
            Span::styled(session.status.label(), status_style),
        ]),
        Line::from(vec![label("Uptime"), Span::raw(uptime(session))]),
        Line::from(vec![label("Version"), Span::raw(session.version.clone())]),
    ];

    let region = match (&session.region, session.latency_ms) {
        (Some(region), Some(latency)) => format!("{region} · {latency}ms"),
        (Some(region), None) => region.clone(),
        (None, Some(latency)) => format!("{latency}ms"),
        (None, None) => "—".to_string(),
    };

    let right = vec![
        Line::from(vec![label("Server"), Span::raw(session.server.clone())]),
        Line::from(vec![label("Region"), Span::raw(region)]),
        Line::from(vec![
            label("Inspect"),
            match &session.inspect_url {
                Some(url) => Span::styled(url.clone(), Style::default().fg(Color::Cyan)),
                None => Span::styled("disabled", Style::default().fg(Color::DarkGray)),
            },
        ]),
    ];

    frame.render_widget(Paragraph::new(left), columns[0]);
    frame.render_widget(Paragraph::new(right), columns[1]);
}

/// Fixed-width dim label so the two header columns line up.
fn label(text: &str) -> Span<'static> {
    Span::styled(format!("{text:<9}"), Style::default().fg(Color::DarkGray))
}

fn uptime(session: &Session) -> String {
    let elapsed = Utc::now()
        .signed_duration_since(session.started_at)
        .num_seconds()
        .max(0);
    format!(
        "{:02}:{:02}:{:02}",
        elapsed / 3600,
        (elapsed % 3600) / 60,
        elapsed % 60
    )
}

// ── tunnels ───────────────────────────────────────────────────────────────────

fn draw_tunnels(frame: &mut Frame, area: Rect, session: &Session) {
    let block = Block::default().borders(Borders::ALL).title(" Tunnels ");

    if session.tunnels.is_empty() {
        let waiting = Paragraph::new(Line::from(Span::styled(
            "registering…",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block);
        frame.render_widget(waiting, area);
        return;
    }

    let rows: Vec<Row> = session
        .tunnels
        .iter()
        .map(|tunnel| {
            let health = match tunnel.healthy {
                Some(true) => Span::styled("● healthy", Style::default().fg(Color::Green)),
                Some(false) => Span::styled("● unhealthy", Style::default().fg(Color::Red)),
                None => Span::styled("", Style::default()),
            };
            Row::new(vec![
                Cell::from(Span::styled(
                    tunnel.proto.to_uppercase(),
                    Style::default().fg(Color::Yellow).bold(),
                )),
                Cell::from(Span::styled(
                    tunnel.name.clone(),
                    Style::default().fg(Color::DarkGray),
                )),
                Cell::from(Span::styled(
                    tunnel.public_url.clone(),
                    Style::default().fg(Color::Green).bold(),
                )),
                Cell::from(Span::styled(
                    format!("→ {}", tunnel.local),
                    Style::default().fg(Color::DarkGray),
                )),
                Cell::from(health),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(12),
            Constraint::Min(24),
            Constraint::Length(24),
            Constraint::Length(12),
        ],
    )
    .block(block);

    frame.render_widget(table, area);
}

// ── traffic ───────────────────────────────────────────────────────────────────

fn draw_traffic(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title(" Traffic ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let columns = Layout::horizontal([Constraint::Min(40), Constraint::Length(24)]).split(inner);

    let stats = app.inspector.stats.snapshot();
    let (p50, p90) = app.inspector.latency_percentiles().unwrap_or((0, 0));

    let counters = vec![
        Line::from(vec![
            label("Conns"),
            Span::styled(
                stats.conns_open.to_string(),
                Style::default().fg(Color::Cyan).bold(),
            ),
            Span::styled(" open · ", Style::default().fg(Color::DarkGray)),
            Span::raw(stats.conns_total.to_string()),
            Span::styled(" total    ", Style::default().fg(Color::DarkGray)),
            label("Requests"),
            Span::raw(stats.requests_total.to_string()),
        ]),
        Line::from(vec![
            label("Traffic"),
            Span::styled("↑ ", Style::default().fg(Color::DarkGray)),
            Span::raw(format_bytes(stats.bytes_to_tunnel)),
            Span::styled("  ↓ ", Style::default().fg(Color::DarkGray)),
            Span::raw(format_bytes(stats.bytes_to_local)),
            Span::styled("    ", Style::default()),
            label("Latency"),
            Span::raw(format!("p50 {p50}ms · p90 {p90}ms")),
        ]),
    ];
    frame.render_widget(Paragraph::new(counters), columns[0]);

    let peak = app.rps_history.iter().copied().max().unwrap_or(1).max(1);
    let sparkline = Sparkline::default()
        .block(Block::default().title(Span::styled("req/s", Style::default().fg(Color::DarkGray))))
        .data(&app.rps_history)
        .max(peak)
        .style(Style::default().fg(Color::Cyan));
    frame.render_widget(sparkline, columns[1]);
}

// ── request log ───────────────────────────────────────────────────────────────

fn draw_requests(frame: &mut Frame, area: Rect, app: &App) {
    let exchanges = app.inspector.exchanges();
    let follow = if app.follow { "" } else { " (paused)" };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Requests{follow} "));

    if exchanges.is_empty() {
        let hint = Paragraph::new(Line::from(Span::styled(
            "waiting for traffic…",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block);
        frame.render_widget(hint, area);
        return;
    }

    let capacity = block.inner(area).height as usize;
    let rows: Vec<Row> = exchanges
        .iter()
        .skip(app.offset)
        .take(capacity)
        .map(|exchange| request_row(exchange))
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),  // time
            Constraint::Length(7),  // method
            Constraint::Min(20),    // path
            Constraint::Length(5),  // status
            Constraint::Length(9),  // duration
            Constraint::Length(22), // client
        ],
    )
    .block(block);

    frame.render_widget(table, area);
}

fn request_row(exchange: &Exchange) -> Row<'static> {
    let time = exchange
        .started_at
        .with_timezone(&chrono::Local)
        .format("%H:%M:%S")
        .to_string();

    let path = if exchange.replayed {
        format!("{} ↻", exchange.path)
    } else {
        exchange.path.clone()
    };

    Row::new(vec![
        Cell::from(Span::styled(time, Style::default().fg(Color::DarkGray))),
        Cell::from(Span::styled(
            exchange.method.clone(),
            method_style(&exchange.method),
        )),
        Cell::from(path),
        Cell::from(Span::styled(
            exchange.status.to_string(),
            status_style(exchange.status),
        )),
        Cell::from(Span::styled(
            format!("{} ms", exchange.duration_ms),
            duration_style(exchange.duration_ms),
        )),
        Cell::from(Span::styled(
            exchange.client_addr.clone(),
            Style::default().fg(Color::DarkGray),
        )),
    ])
}

fn method_style(method: &str) -> Style {
    let color = match method {
        "GET" => Color::Cyan,
        "POST" => Color::Yellow,
        "PUT" | "PATCH" => Color::Magenta,
        "DELETE" => Color::Red,
        _ => Color::White,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn status_style(status: u16) -> Style {
    let style = Style::default();
    match status {
        200..=299 => style.fg(Color::Green).add_modifier(Modifier::BOLD),
        300..=399 => style.fg(Color::Cyan),
        400..=499 => style.fg(Color::Yellow).add_modifier(Modifier::BOLD),
        500..=599 => style.fg(Color::Red).add_modifier(Modifier::BOLD),
        _ => style.fg(Color::White),
    }
}

fn duration_style(duration_ms: u64) -> Style {
    let color = match duration_ms {
        0..=99 => Color::Green,
        100..=999 => Color::Yellow,
        _ => Color::Red,
    };
    Style::default().fg(color)
}

// ── log pane ──────────────────────────────────────────────────────────────────

fn draw_logs(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title(" Logs ");
    let capacity = block.inner(area).height as usize;

    let logs = app.inspector.logs();
    let lines: Vec<Line> = logs
        .iter()
        .rev()
        .skip(app.offset)
        .take(capacity)
        .map(|line| Line::from(Span::raw(line.clone())))
        .collect();

    let content = if lines.is_empty() {
        vec![Line::from(Span::styled(
            "no log output",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        lines
    };

    frame.render_widget(Paragraph::new(content).block(block), area);
}

// ── footer ────────────────────────────────────────────────────────────────────

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let pane = if app.show_logs { "requests" } else { "logs" };
    let follow = if app.follow { "pause" } else { "follow" };

    let hints = vec![
        key("q"),
        Span::raw(" quit  "),
        key("↑↓"),
        Span::raw(" scroll  "),
        key("f"),
        Span::raw(format!(" {follow}  ")),
        key("c"),
        Span::raw(" clear  "),
        key("l"),
        Span::raw(format!(" {pane}")),
    ];

    frame.render_widget(
        Paragraph::new(Line::from(hints)).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn key(name: &str) -> Span<'static> {
    Span::styled(
        name.to_string(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
}
