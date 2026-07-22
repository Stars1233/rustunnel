//! Full-screen terminal UI.
//!
//! Takes over the terminal for the lifetime of the session and shows what the
//! old startup box could not: live connection state, per-tunnel health, traffic
//! counters, and a scrolling log of every request flowing through the tunnel.
//!
//! The UI only ever *reads* [`Inspector`] state, so it can be skipped entirely
//! — `--json`, `--no-tui` and non-TTY runs keep the original line-based output
//! and the tunnel behaves identically.

mod ui;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;

use crate::inspect::{Exchange, Inspector};

/// True while the terminal UI owns the screen. Console output (spinner,
/// startup box, tracing) must route elsewhere while this is set.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Where tracing output goes while the UI is active.
static LOG_SINK: OnceLock<Arc<Inspector>> = OnceLock::new();

/// True while the terminal UI owns the screen.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
}

/// Route tracing output into this inspector's log buffer while the UI runs.
pub fn set_log_sink(inspector: Arc<Inspector>) {
    let _ = LOG_SINK.set(inspector);
}

/// `tracing` writer that keeps diagnostics off the screen while the UI is up.
///
/// Without this, every `warn!` would punch a hole through the rendered frame.
/// Captured lines are shown in the UI's log pane instead (`l`).
pub struct LogWriter;

impl io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !active() {
            return io::Write::write(&mut io::stderr(), buf);
        }
        if let Some(inspector) = LOG_SINK.get() {
            for line in String::from_utf8_lossy(buf).lines() {
                if !line.trim().is_empty() {
                    inspector.push_log(line.to_string());
                }
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if active() {
            return Ok(());
        }
        io::Write::flush(&mut io::stderr())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogWriter {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter
    }
}

/// Redraw interval. Fast enough to feel live, slow enough to stay cheap.
const TICK: Duration = Duration::from_millis(250);

/// Seconds of request-rate history kept for the sparkline.
const RPS_HISTORY: usize = 60;

/// How long the input thread waits for a key before re-checking for shutdown.
const INPUT_POLL: Duration = Duration::from_millis(150);

/// UI state. Everything else is read live from the [`Inspector`].
pub struct App {
    inspector: Arc<Inspector>,
    /// Index of the topmost visible request row (0 = newest).
    offset: usize,
    /// Stick to the newest request as traffic arrives.
    follow: bool,
    /// Show the log pane instead of the request list.
    show_logs: bool,
    /// Completed requests per second, oldest first.
    rps_history: Vec<u64>,
    /// Requests counted in the current second.
    rps_current: u64,
    quit: bool,
}

impl App {
    fn new(inspector: Arc<Inspector>) -> Self {
        Self {
            inspector,
            offset: 0,
            follow: true,
            show_logs: false,
            rps_history: vec![0; RPS_HISTORY],
            rps_current: 0,
            quit: false,
        }
    }

    fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('c') => {
                self.inspector.clear();
                self.offset = 0;
            }
            KeyCode::Char('f') => {
                self.follow = !self.follow;
                if self.follow {
                    self.offset = 0;
                }
            }
            KeyCode::Char('l') => self.show_logs = !self.show_logs,
            KeyCode::Up | KeyCode::Char('k') => self.scroll_back(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_forward(1),
            KeyCode::PageUp => self.scroll_back(10),
            KeyCode::PageDown => self.scroll_forward(10),
            KeyCode::Home | KeyCode::Char('g') => {
                self.offset = 0;
                self.follow = true;
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.offset = self.max_offset();
                self.follow = false;
            }
            _ => {}
        }
    }

    /// Scroll towards older entries (further from the newest).
    fn scroll_back(&mut self, lines: usize) {
        self.offset = (self.offset + lines).min(self.max_offset());
        self.follow = false;
    }

    /// Scroll back towards the newest entries.
    fn scroll_forward(&mut self, lines: usize) {
        self.offset = self.offset.saturating_sub(lines);
        if self.offset == 0 {
            self.follow = true;
        }
    }

    fn max_offset(&self) -> usize {
        let len = if self.show_logs {
            self.inspector.logs().len()
        } else {
            self.inspector.exchanges().len()
        };
        len.saturating_sub(1)
    }

    fn on_exchange(&mut self, _exchange: &Exchange) {
        self.rps_current += 1;
        if self.follow {
            self.offset = 0;
        } else {
            // Keep the viewport anchored on the same rows as newer ones arrive.
            self.offset = (self.offset + 1).min(self.max_offset());
        }
    }

    /// Roll the per-second request counter into the sparkline history.
    fn on_second(&mut self) {
        self.rps_history.remove(0);
        self.rps_history.push(self.rps_current);
        self.rps_current = 0;
    }
}

/// Run the terminal UI until the user quits or the session ends.
///
/// Always restores the terminal, including on panic.
pub async fn run(inspector: Arc<Inspector>) -> io::Result<()> {
    let mut terminal = ratatui::init();
    set_active(true);

    // ratatui::init() installs a restoring panic hook; chain ours after it so
    // the terminal is sane even if that behaviour ever changes.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        set_active(false);
        ratatui::restore();
        previous_hook(info);
    }));

    let result = run_loop(&mut terminal, inspector).await;

    set_active(false);
    ratatui::restore();
    result
}

async fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    inspector: Arc<Inspector>,
) -> io::Result<()> {
    let mut app = App::new(Arc::clone(&inspector));
    let mut exchanges = inspector.subscribe();

    // crossterm's event API is blocking, so it lives on its own thread and
    // feeds the async loop through a channel.
    let (input_tx, mut input_rx) = mpsc::channel::<Event>(64);
    let stop_input = Arc::new(AtomicBool::new(false));
    let input_thread = std::thread::spawn({
        let stop = Arc::clone(&stop_input);
        move || {
            while !stop.load(Ordering::Relaxed) {
                match event::poll(INPUT_POLL) {
                    Ok(true) => match event::read() {
                        Ok(ev) => {
                            if input_tx.blocking_send(ev).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    },
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        }
    });

    let mut tick = tokio::time::interval(TICK);
    let mut second = tokio::time::interval(Duration::from_secs(1));

    terminal.draw(|frame| ui::draw(frame, &app))?;

    while !app.quit {
        tokio::select! {
            Some(event) = input_rx.recv() => {
                match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        app.on_key(key.code, key.modifiers);
                    }
                    // Redraw on resize; other events are ignored.
                    Event::Resize(_, _) => {}
                    _ => continue,
                }
            }
            result = exchanges.recv() => {
                match result {
                    Ok(exchange) => app.on_exchange(&exchange),
                    // Lagging just means we missed a few rows in the counter.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = second.tick() => app.on_second(),
            _ = tick.tick() => {}
            _ = inspector.shutdown_signal() => break,
        }

        terminal.draw(|frame| ui::draw(frame, &app))?;
    }

    // Quitting the UI ends the session.
    inspector.request_shutdown();
    stop_input.store(true, Ordering::Relaxed);
    let _ = input_thread.join();
    Ok(())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect::Body;
    use chrono::Utc;
    use uuid::Uuid;

    fn app_with(count: usize) -> App {
        let inspector = Inspector::new(true, "edge.test:4040".into(), None);
        for _ in 0..count {
            inspector.record(Exchange {
                id: 0,
                conn_id: Uuid::nil(),
                tunnel: "web".into(),
                client_addr: "203.0.113.1:1".into(),
                method: "GET".into(),
                path: "/".into(),
                host: None,
                status: 200,
                request_headers: vec![],
                response_headers: vec![],
                request_body: Body::default(),
                response_body: Body::default(),
                duration_ms: 1,
                started_at: Utc::now(),
                replayed: false,
            });
        }
        App::new(inspector)
    }

    #[test]
    fn quit_keys_set_the_quit_flag() {
        for (code, modifiers) in [
            (KeyCode::Char('q'), KeyModifiers::NONE),
            (KeyCode::Esc, KeyModifiers::NONE),
            (KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let mut app = app_with(0);
            app.on_key(code, modifiers);
            assert!(app.quit, "{code:?} should quit");
        }
    }

    #[test]
    fn plain_c_clears_instead_of_quitting() {
        let mut app = app_with(3);
        app.on_key(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(!app.quit);
        assert!(app.inspector.exchanges().is_empty());
    }

    #[test]
    fn scrolling_disables_follow_and_clamps_to_the_oldest_row() {
        let mut app = app_with(5);
        assert!(app.follow);

        app.on_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.offset, 1);
        assert!(!app.follow, "scrolling back stops following");

        app.on_key(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(app.offset, 4, "clamped to the oldest row");

        app.on_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.offset, 3);
        assert!(!app.follow);

        app.on_key(KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(app.offset, 0);
        assert!(app.follow, "jumping to the newest resumes following");
    }

    #[test]
    fn new_requests_keep_the_viewport_anchored_when_not_following() {
        let mut app = app_with(5);
        app.on_key(KeyCode::Up, KeyModifiers::NONE); // offset 1, follow off
        let before = app.offset;

        let inspector = Arc::clone(&app.inspector);
        let exchanges = inspector.exchanges();
        app.on_exchange(&exchanges[0]);

        assert_eq!(app.offset, before + 1, "the same rows stay in view");
        assert_eq!(app.rps_current, 1);
    }

    #[test]
    fn following_pins_the_view_to_the_newest_request() {
        let mut app = app_with(5);
        let exchanges = app.inspector.exchanges();
        app.on_exchange(&exchanges[0]);
        assert_eq!(app.offset, 0);
    }

    #[test]
    fn per_second_rollup_shifts_the_sparkline_history() {
        let mut app = app_with(0);
        app.rps_current = 7;
        app.on_second();
        assert_eq!(app.rps_history.len(), RPS_HISTORY);
        assert_eq!(*app.rps_history.last().unwrap(), 7);
        assert_eq!(app.rps_current, 0);
    }

    #[test]
    fn log_pane_toggles_and_uses_its_own_length_for_bounds() {
        let mut app = app_with(2);
        app.inspector.push_log("one".into());
        app.on_key(KeyCode::Char('l'), KeyModifiers::NONE);
        assert!(app.show_logs);
        assert_eq!(app.max_offset(), 0, "one log line, so no scrolling");
    }

    // ── rendering ────────────────────────────────────────────────────────

    /// Render one frame into an in-memory terminal and return it as text.
    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| super::ui::draw(frame, app)).unwrap();

        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn full_frame_shows_session_tunnel_and_requests() {
        let app = app_with(3);
        app.inspector
            .set_status(crate::inspect::SessionStatus::Online);
        app.inspector.set_latency(57);
        app.inspector
            .set_inspect_url("http://127.0.0.1:4040".to_string());
        app.inspector.set_tunnels(vec![crate::inspect::TunnelInfo {
            name: "web".into(),
            proto: "http".into(),
            local: "localhost:3000".into(),
            public_url: "http://bb176fb6.eu.edge.rustunnel.com".into(),
            healthy: Some(true),
        }]);

        let frame = render(&app, 110, 26);
        // `cargo test -p rustunnel-client full_frame -- --nocapture` prints the
        // layout, which is the quickest way to eyeball a UI change.
        println!("{frame}");

        // Header
        assert!(frame.contains("rustunnel"), "{frame}");
        assert!(frame.contains("online"), "{frame}");
        assert!(frame.contains("57ms"), "latency is shown: {frame}");
        assert!(frame.contains("http://127.0.0.1:4040"), "{frame}");
        // Tunnels
        assert!(frame.contains("HTTP"), "{frame}");
        assert!(frame.contains("bb176fb6.eu.edge.rustunnel.com"), "{frame}");
        assert!(frame.contains("→ localhost:3000"), "{frame}");
        assert!(frame.contains("healthy"), "{frame}");
        // Traffic + request log + footer
        assert!(frame.contains("req/s"), "{frame}");
        assert!(frame.contains("Requests"), "{frame}");
        assert!(frame.contains("GET"), "{frame}");
        assert!(frame.contains("200"), "{frame}");
        assert!(frame.contains("quit"), "footer hints: {frame}");
    }

    #[test]
    fn empty_state_and_narrow_terminals_render_without_panicking() {
        let app = app_with(0);
        let frame = render(&app, 100, 24);
        assert!(frame.contains("waiting for traffic"), "{frame}");
        assert!(frame.contains("registering"), "{frame}");

        // Small and awkward sizes must not panic (ratatui truncates instead).
        for (width, height) in [(20, 10), (40, 14), (200, 60), (10, 5)] {
            render(&app, width, height);
        }
    }

    #[test]
    fn paused_scrolling_is_reflected_in_the_frame() {
        let mut app = app_with(3);
        app.on_key(KeyCode::Up, KeyModifiers::NONE);
        let frame = render(&app, 100, 24);
        assert!(frame.contains("(paused)"), "{frame}");
        assert!(frame.contains("follow"), "footer offers resuming: {frame}");
    }

    #[test]
    fn log_pane_renders_captured_lines() {
        let mut app = app_with(1);
        app.inspector.push_log("WARN heartbeat timeout".into());
        app.on_key(KeyCode::Char('l'), KeyModifiers::NONE);
        let frame = render(&app, 100, 24);
        assert!(frame.contains("Logs"), "{frame}");
        assert!(frame.contains("WARN heartbeat timeout"), "{frame}");
    }
}
