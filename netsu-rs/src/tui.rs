//! `netsu tui`: a ratatui launcher + live dashboard. Pick a mode, watch it run
//! live (per-stream throughput / probe latency), then read the summary — no
//! flags to memorize. Throughput (TCP loopback) works without extra features;
//! the mux lab modes require `--features iroh`.

use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Cell, Gauge, List, ListItem, ListState, Paragraph, Row, Sparkline, Table,
};
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Semantic palette (Catppuccin Mocha), threaded everywhere — never `Color::x`
/// ad hoc in render code.
struct Theme {
    base: Color,
    surface: Color,
    text: Color,
    subtext: Color,
    accent: Color,
    blue: Color,
    green: Color,
    yellow: Color,
    peach: Color,
}
const T: Theme = Theme {
    base: Color::Rgb(0x1e, 0x1e, 0x2e),
    surface: Color::Rgb(0x31, 0x32, 0x44),
    text: Color::Rgb(0xcd, 0xd6, 0xf4),
    subtext: Color::Rgb(0xa6, 0xad, 0xc8),
    accent: Color::Rgb(0xcb, 0xa6, 0xf7),
    blue: Color::Rgb(0x89, 0xb4, 0xfa),
    green: Color::Rgb(0xa6, 0xe3, 0xa1),
    yellow: Color::Rgb(0xf9, 0xe2, 0xaf),
    peach: Color::Rgb(0xfa, 0xb3, 0x87),
};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A menu entry: a label plus the run it launches.
struct MenuItem {
    label: &'static str,
    hint: &'static str,
    mode: Mode,
}

#[derive(Clone, Copy)]
enum Mode {
    ThroughputUpload,
    ThroughputReverse,
    #[cfg(feature = "iroh")]
    MuxInputFile,
    #[cfg(feature = "iroh")]
    MuxMixed,
}

/// One live row in the dashboard (unified across throughput/mux so the App
/// state carries no feature-gated types).
#[derive(Clone)]
struct LiveRow {
    label: String,
    priority: Option<i32>,
    mbps: f64,
    measured: bool,
}

/// A finished run's human summary (plain data — no feature-gated types).
#[derive(Clone, Default)]
struct Summary {
    title: String,
    lines: Vec<String>,
    cli: String,
}

/// Messages from the running task to the UI loop.
enum RunMsg {
    Live { elapsed_ms: u64, rows: Vec<LiveRow> },
    Done(Summary),
}

enum Screen {
    Home,
    Running,
    Summary,
}

struct App {
    screen: Screen,
    items: Vec<MenuItem>,
    menu: ListState,
    duration_s: u64,
    spinner: usize,
    // running state
    run_rx: Option<UnboundedReceiver<RunMsg>>,
    rows: Vec<LiveRow>,
    spark: Vec<u64>,
    elapsed_ms: u64,
    running_title: String,
    summary: Summary,
    quit: bool,
}

/// Entry point for `netsu tui`.
pub async fn run() -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let result = App::new().run(&mut terminal).await;
    ratatui::restore();
    result
}

impl App {
    fn new() -> Self {
        #[allow(unused_mut)] // `mut` used only when the iroh mux items are pushed
        let mut items = vec![
            MenuItem {
                label: "TCP throughput — upload",
                hint: "loopback iperf3-style send test",
                mode: Mode::ThroughputUpload,
            },
            MenuItem {
                label: "TCP throughput — reverse",
                hint: "loopback download (server sends)",
                mode: Mode::ThroughputReverse,
            },
        ];
        #[cfg(feature = "iroh")]
        {
            items.push(MenuItem {
                label: "Mux — input under file load",
                hint: "does the high-priority probe stay low-latency?",
                mode: Mode::MuxInputFile,
            });
            items.push(MenuItem {
                label: "Mux — mixed workloads",
                hint: "input + clipboard + cast + file, graded priority",
                mode: Mode::MuxMixed,
            });
        }
        let mut menu = ListState::default();
        menu.select(Some(0));
        App {
            screen: Screen::Home,
            items,
            menu,
            duration_s: 5,
            spinner: 0,
            run_rx: None,
            rows: Vec::new(),
            spark: Vec::new(),
            elapsed_ms: 0,
            running_title: String::new(),
            summary: Summary::default(),
            quit: false,
        }
    }

    async fn run(&mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
        // Read blocking input on its own thread → channel, so the async loop can
        // also service ticks and the running task's messages.
        let (input_tx, mut input_rx) = unbounded_channel::<Event>();
        std::thread::spawn(move || {
            while let Ok(ev) = event::read() {
                if input_tx.send(ev).is_err() {
                    break;
                }
            }
        });
        let mut tick = tokio::time::interval(Duration::from_millis(100));

        loop {
            terminal.draw(|f| self.render(f))?;
            if self.quit {
                break;
            }
            tokio::select! {
                Some(ev) = input_rx.recv() => self.on_event(ev),
                _ = tick.tick() => { self.spinner = self.spinner.wrapping_add(1); }
                msg = recv_run(&mut self.run_rx), if self.run_rx.is_some() => {
                    match msg {
                        Some(m) => self.on_run_msg(m),
                        None => { self.run_rx = None; }
                    }
                }
            }
        }
        Ok(())
    }

    fn on_event(&mut self, ev: Event) {
        let Event::Key(key) = ev else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }
        match self.screen {
            Screen::Home => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
                KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
                KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Right => {
                    self.duration_s = (self.duration_s + 1).min(60);
                }
                KeyCode::Char('-') | KeyCode::Left => {
                    self.duration_s = self.duration_s.saturating_sub(1).max(1);
                }
                KeyCode::Enter => self.start_selected(),
                _ => {}
            },
            Screen::Running => {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    // Abort: drop the receiver; the task is detached and harmless.
                    self.run_rx = None;
                    self.screen = Screen::Home;
                }
            }
            Screen::Summary => match key.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Esc => self.screen = Screen::Home,
                KeyCode::Char('r') | KeyCode::Enter => self.start_selected(),
                _ => {}
            },
        }
    }

    fn move_sel(&mut self, delta: i32) {
        let n = self.items.len() as i32;
        let cur = self.menu.selected().unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(n);
        self.menu.select(Some(next as usize));
    }

    fn on_run_msg(&mut self, msg: RunMsg) {
        match msg {
            RunMsg::Live { elapsed_ms, rows } => {
                self.elapsed_ms = elapsed_ms;
                // Sparkline tracks the largest stream's Mbps (the load).
                let peak = rows.iter().map(|r| r.mbps).fold(0.0, f64::max);
                self.spark.push(peak as u64);
                if self.spark.len() > 120 {
                    self.spark.remove(0);
                }
                self.rows = rows;
            }
            RunMsg::Done(summary) => {
                self.summary = summary;
                self.screen = Screen::Summary;
                self.run_rx = None;
            }
        }
    }

    fn start_selected(&mut self) {
        let idx = self.menu.selected().unwrap_or(0);
        let Some(item) = self.items.get(idx) else {
            return;
        };
        self.rows.clear();
        self.spark.clear();
        self.elapsed_ms = 0;
        self.running_title = item.label.to_string();
        self.screen = Screen::Running;
        let (tx, rx) = unbounded_channel();
        self.run_rx = Some(rx);
        spawn_run(item.mode, self.duration_s, tx);
    }

    // ---- rendering ----

    fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        f.render_widget(Block::default().style(Style::new().bg(T.base)), area);
        match self.screen {
            Screen::Home => self.render_home(f, area),
            Screen::Running => self.render_running(f, area),
            Screen::Summary => self.render_summary(f, area),
        }
    }

    fn render_home(&mut self, f: &mut Frame, area: Rect) {
        let [title, body, help] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .margin(1)
        .areas(area);

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("netsu", Style::new().fg(T.accent).bold()),
                Span::styled(
                    "  network speed + multiplexing lab",
                    Style::new().fg(T.subtext),
                ),
            ]))
            .block(rounded("welcome")),
            title,
        );

        let items: Vec<ListItem> = self
            .items
            .iter()
            .map(|it| {
                ListItem::new(Line::from(vec![
                    Span::styled(it.label, Style::new().fg(T.text)),
                    Span::raw("  "),
                    Span::styled(it.hint, Style::new().fg(T.subtext).italic()),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(rounded(&format!(
                "choose a run   (duration {}s)",
                self.duration_s
            )))
            .highlight_style(
                Style::new()
                    .fg(T.base)
                    .bg(T.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, body, &mut self.menu);

        f.render_widget(
            help_bar(&[
                ("↑/↓", "select"),
                ("←/→", "duration"),
                ("enter", "run"),
                ("q", "quit"),
            ]),
            help,
        );
    }

    fn render_running(&mut self, f: &mut Frame, area: Rect) {
        let [header, spark, table_area, help] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(7),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .margin(1)
        .areas(area);

        let total = (self.duration_s * 1000).max(1);
        let ratio = (self.elapsed_ms as f64 / total as f64).clamp(0.0, 1.0);
        let spin = SPINNER[self.spinner % SPINNER.len()];
        f.render_widget(
            Gauge::default()
                .block(rounded(&format!("{spin} {}", self.running_title)))
                .gauge_style(Style::new().fg(T.green).bg(T.surface))
                .ratio(ratio)
                .label(format!(
                    "{:.1}s / {}s",
                    self.elapsed_ms as f64 / 1000.0,
                    self.duration_s
                )),
            header,
        );

        f.render_widget(
            Sparkline::default()
                .block(rounded("throughput (Mbps)"))
                .data(&self.spark)
                .style(Style::new().fg(T.blue)),
            spark,
        );

        let rows: Vec<Row> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let prio = r
                    .priority
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".into());
                let role = if r.measured { "probe" } else { "load" };
                let role_color = if r.measured { T.yellow } else { T.subtext };
                Row::new(vec![
                    Cell::from(r.label.clone()).style(Style::new().fg(T.text)),
                    Cell::from(prio).style(Style::new().fg(T.peach)),
                    Cell::from(role).style(Style::new().fg(role_color)),
                    Cell::from(format!("{:>8.1}", r.mbps)).style(Style::new().fg(T.blue)),
                ])
                .style(Style::new().bg(if i % 2 == 0 {
                    T.base
                } else {
                    T.surface
                }))
            })
            .collect();
        let table = Table::new(
            rows,
            [
                Constraint::Min(10),
                Constraint::Length(6),
                Constraint::Length(7),
                Constraint::Length(10),
            ],
        )
        .header(
            Row::new(vec!["stream", "prio", "role", "Mbps"])
                .style(Style::new().fg(T.base).bg(T.accent).bold()),
        )
        .block(rounded("streams"));
        f.render_widget(table, table_area);

        f.render_widget(help_bar(&[("q/esc", "abort")]), help);
    }

    fn render_summary(&mut self, f: &mut Frame, area: Rect) {
        let [title, body, cli, help] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .margin(1)
        .areas(area);

        f.render_widget(
            Paragraph::new(Span::styled(
                &self.summary.title,
                Style::new().fg(T.green).bold(),
            ))
            .block(rounded("done")),
            title,
        );
        let lines: Vec<Line> = self
            .summary
            .lines
            .iter()
            .map(|l| Line::from(Span::styled(l.clone(), Style::new().fg(T.text))))
            .collect();
        f.render_widget(Paragraph::new(lines).block(rounded("result")), body);
        f.render_widget(
            Paragraph::new(Span::styled(
                &self.summary.cli,
                Style::new().fg(T.subtext).italic(),
            ))
            .block(rounded("equivalent CLI"))
            .alignment(Alignment::Left),
            cli,
        );
        f.render_widget(
            help_bar(&[("r", "rerun"), ("esc", "home"), ("q", "quit")]),
            help,
        );
    }
}

fn rounded(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(T.surface))
        .title(Span::styled(title.to_string(), Style::new().fg(T.subtext)))
        .style(Style::new().bg(T.base))
}

fn help_bar(keys: &[(&str, &str)]) -> Paragraph<'static> {
    let mut spans = Vec::new();
    for (k, d) in keys {
        spans.push(Span::styled(
            format!(" {k} "),
            Style::new().fg(T.base).bg(T.accent).bold(),
        ));
        spans.push(Span::styled(format!(" {d}   "), Style::new().fg(T.subtext)));
    }
    Paragraph::new(Line::from(spans))
}

/// Await the optional run receiver (only polled when it is `Some`).
async fn recv_run(rx: &mut Option<UnboundedReceiver<RunMsg>>) -> Option<RunMsg> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

fn spawn_run(mode: Mode, duration_s: u64, tx: UnboundedSender<RunMsg>) {
    match mode {
        Mode::ThroughputUpload => spawn_throughput(false, duration_s, tx),
        Mode::ThroughputReverse => spawn_throughput(true, duration_s, tx),
        #[cfg(feature = "iroh")]
        Mode::MuxInputFile => spawn_mux(MuxKind::InputFile, duration_s, tx),
        #[cfg(feature = "iroh")]
        Mode::MuxMixed => spawn_mux(MuxKind::Mixed, duration_s, tx),
    }
}

fn spawn_throughput(reverse: bool, duration_s: u64, tx: UnboundedSender<RunMsg>) {
    use netsu::client::{ClientOptions, run_client};
    use netsu::server::{ServerOptions, start_server};
    tokio::spawn(async move {
        let server = match start_server(ServerOptions {
            port: 0,
            ..Default::default()
        })
        .await
        {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(RunMsg::Done(Summary {
                    title: "server failed".into(),
                    lines: vec![e.to_string()],
                    cli: String::new(),
                }));
                return;
            }
        };
        let port = server.port;
        let tx_interval = tx.clone();
        let start = std::time::Instant::now();
        let on_interval = Box::new(move |r: netsu::stats::IntervalReport| {
            let _ = tx_interval.send(RunMsg::Live {
                elapsed_ms: start.elapsed().as_millis() as u64,
                rows: vec![LiveRow {
                    label: "throughput".into(),
                    priority: None,
                    mbps: r.bits_per_second / 1e6,
                    measured: false,
                }],
            });
        });
        let opts = ClientOptions {
            port,
            duration: duration_s as u32,
            reverse,
            ..Default::default()
        };
        let result = run_client("127.0.0.1", opts, Some(on_interval)).await;
        server.close().await;
        let summary = match result {
            Ok(r) => Summary {
                title: "throughput test complete".into(),
                lines: vec![
                    format!("sent      {:.1} Mbit/s", r.send_bits_per_second / 1e6),
                    format!("received  {:.1} Mbit/s", r.receive_bits_per_second / 1e6),
                    format!(
                        "bytes     {} sent / {} received",
                        r.sent_bytes, r.received_bytes
                    ),
                ],
                cli: format!(
                    "netsu server   |   netsu client 127.0.0.1 -t {duration_s}{}",
                    if reverse { " -R" } else { "" }
                ),
            },
            Err(e) => Summary {
                title: "test failed".into(),
                lines: vec![e.to_string()],
                cli: String::new(),
            },
        };
        let _ = tx.send(RunMsg::Done(summary));
    });
}

#[cfg(feature = "iroh")]
enum MuxKind {
    InputFile,
    Mixed,
}

#[cfg(feature = "iroh")]
fn spawn_mux(kind: MuxKind, duration_s: u64, tx: UnboundedSender<RunMsg>) {
    use netsu::mux::config::{RunConfig, ScenarioName};
    use netsu::mux::protocol::MUX_ALPN;
    use netsu::mux::runner::{LiveSnapshot, run_with_live};
    use netsu::mux::{receiver, result::MuxResult};
    use netsu::p2p::endpoint::LocalPair;

    let (scenario, scenario_cli) = match kind {
        MuxKind::InputFile => (ScenarioName::InputFile, "input-file"),
        MuxKind::Mixed => (ScenarioName::Mixed, "mixed"),
    };
    let dur = Duration::from_secs(duration_s);
    let config = RunConfig {
        scenario,
        duration: dur,
        warmup: dur / 5,
        cooldown: dur / 10,
        ..Default::default()
    };

    tokio::spawn(async move {
        let pair = match LocalPair::connect(MUX_ALPN).await {
            Ok(p) => p,
            Err(e) => {
                let _ = tx.send(RunMsg::Done(Summary {
                    title: "iroh setup failed".into(),
                    lines: vec![format!("{e:#}")],
                    cli: String::new(),
                }));
                return;
            }
        };
        let server_conn = pair.server_connection.clone();
        let serve = tokio::spawn(async move { receiver::serve(server_conn).await });

        // Forward live snapshots as unified LiveRows.
        let (live_tx, mut live_rx) = unbounded_channel::<LiveSnapshot>();
        let tx_fwd = tx.clone();
        let fwd = tokio::spawn(async move {
            while let Some(snap) = live_rx.recv().await {
                let secs = (snap.elapsed_ms as f64 / 1000.0).max(0.001);
                let rows = snap
                    .streams
                    .iter()
                    .map(|s| LiveRow {
                        label: format!("{:?}#{}", s.kind, s.index),
                        priority: Some(s.priority),
                        mbps: s.bytes_sent as f64 * 8.0 / 1e6 / secs,
                        measured: s.measured,
                    })
                    .collect();
                let _ = tx_fwd.send(RunMsg::Live {
                    elapsed_ms: snap.elapsed_ms,
                    rows,
                });
            }
        });

        let outcome = run_with_live(&pair.client_connection, &config, Some(live_tx)).await;
        let _ = serve.await;
        let _ = fwd.await;
        pair.close().await;

        let summary =
            match outcome {
                Ok(o) => {
                    let result = MuxResult::from_outcome(&o, config.seed);
                    let mut lines = Vec::new();
                    for s in &result.streams {
                        match &s.latency {
                        Some(l) => lines.push(format!(
                            "{:<12} prio {:>2}  {:>7.1} Mbps  p50 {:.2}ms p99 {:.2}ms  miss {:.1}%",
                            s.kind, s.priority, s.throughput_mbps,
                            l.p50_us as f64 / 1000.0, l.p99_us as f64 / 1000.0,
                            l.deadline_exceeded_rate * 100.0
                        )),
                        None => lines.push(format!(
                            "{:<12} prio {:>2}  {:>7.1} Mbps  (load)",
                            s.kind, s.priority, s.throughput_mbps
                        )),
                    }
                    }
                    if let Some(p99) = result.aggregate.probe_p99_us {
                        lines.push(String::new());
                        lines.push(format!(
                            "probe p99: {:.2} ms   fairness: {:.3}",
                            p99 as f64 / 1000.0,
                            result.aggregate.jain_fairness
                        ));
                    }
                    Summary {
                        title: "mux run complete".into(),
                        lines,
                        cli: format!(
                            "netsu mux local --scenario {scenario_cli} --duration {duration_s}s"
                        ),
                    }
                }
                Err(e) => Summary {
                    title: "mux run failed".into(),
                    lines: vec![format!("{e:#}")],
                    cli: String::new(),
                },
            };
        let _ = tx.send(RunMsg::Done(summary));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn rendered(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(90, 26)).unwrap();
        terminal.draw(|f| app.render(f)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn home_lists_the_menu() {
        let mut app = App::new();
        let screen = rendered(&mut app);
        assert!(screen.contains("netsu"));
        assert!(screen.contains("TCP throughput"));
        assert!(screen.contains("choose a run"));
    }

    #[test]
    fn running_dashboard_renders_stream_rows() {
        let mut app = App::new();
        app.screen = Screen::Running;
        app.running_title = "Mux — input under file load".into();
        app.elapsed_ms = 500;
        app.spark = vec![10, 40, 90, 120];
        app.rows = vec![
            LiveRow {
                label: "Input#0".into(),
                priority: Some(30),
                mbps: 0.1,
                measured: true,
            },
            LiveRow {
                label: "File#1".into(),
                priority: Some(0),
                mbps: 134.2,
                measured: false,
            },
        ];
        let screen = rendered(&mut app);
        assert!(screen.contains("Input#0"));
        assert!(screen.contains("File#1"));
        assert!(screen.contains("probe"));
        assert!(screen.contains("load"));
        assert!(screen.contains("Mbps"));
    }

    #[test]
    fn summary_shows_lines_and_cli() {
        let mut app = App::new();
        app.screen = Screen::Summary;
        app.summary = Summary {
            title: "mux run complete".into(),
            lines: vec!["Input prio 30  0.1 Mbps  p99 1.80ms".into()],
            cli: "netsu mux local --scenario input-file --duration 5s".into(),
        };
        let screen = rendered(&mut app);
        assert!(screen.contains("mux run complete"));
        assert!(screen.contains("p99 1.80ms"));
        assert!(screen.contains("netsu mux local"));
    }
}
