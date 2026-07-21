//! `netsu tui`: a ratatui launcher + live dashboard whose reason to exist is
//! **cross-device** testing without memorizing flags. You pick a role and a
//! transport; the host publishes a short rendez-key *code*; the other device
//! types that code to join and both ends show a live speed log. Testing against
//! yourself on one machine proves nothing, so the headline flow connects two
//! machines — the local loopback runs are kept only as an offline "lab".
//!
//! The code-based flow (host/join + the kbm sharing screens) needs rendez-key,
//! which lives behind `--features iroh`; a `--features tui`-only build keeps
//! just the loopback lab. The keyboard/mouse screens additionally need
//! `--features input-demo`.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Cell, Gauge, List, ListItem, ListState, Paragraph, Row, Sparkline, Table,
};
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
#[cfg(feature = "iroh")]
use tokio::sync::oneshot;

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
    red: Color,
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
    red: Color::Rgb(0xf3, 0x8b, 0xa8),
};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Which activity a home-menu row launches.
#[derive(Clone, Copy, PartialEq)]
enum Activity {
    /// Host a speed test — pick a transport, publish a code, serve joiners.
    #[cfg(feature = "iroh")]
    HostTest,
    /// Join a speed test — type a host's code and run against it.
    #[cfg(feature = "iroh")]
    JoinTest,
    /// Receive (and optionally inject) a controller's keyboard/mouse.
    #[cfg(feature = "input-demo")]
    KbmControlled,
    /// Capture this device's keyboard/mouse and stream it to a controlled peer.
    #[cfg(feature = "input-demo")]
    KbmController,
    /// Offline lab: loopback upload throughput (server + client in-process).
    LocalUpload,
    /// Offline lab: loopback reverse throughput.
    LocalReverse,
    /// Offline lab: mux — high-priority probe under file load.
    #[cfg(feature = "iroh")]
    LocalMuxInputFile,
    /// Offline lab: mux — mixed graded-priority workloads.
    #[cfg(feature = "iroh")]
    LocalMuxMixed,
}

/// A home-menu row.
struct HomeItem {
    label: &'static str,
    hint: &'static str,
    act: Activity,
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
    ok: bool,
}

/// Messages from a running task to the UI loop.
enum UiMsg {
    /// A client/local run produced a live snapshot.
    Live { elapsed_ms: u64, rows: Vec<LiveRow> },
    /// A client/local run finished.
    Done(Summary),
    /// The hosted server bound and published (or failed to publish) its code.
    #[cfg(feature = "iroh")]
    HostReady {
        code: Option<String>,
        addr_line: String,
    },
    /// The hosted server could not start.
    #[cfg(feature = "iroh")]
    HostFailed(String),
    /// A peer's test pushed one interval of server-side throughput.
    #[cfg(feature = "iroh")]
    HostInterval { mbps: f64 },
    /// A peer's test completed; the host keeps listening for the next one.
    #[cfg(feature = "iroh")]
    HostComplete { line: String },
}

/// What `App::run` asks the outer `run()` to do once the ratatui loop exits.
#[derive(Clone)]
enum PostAction {
    Quit,
    /// Hand off to a keyboard/mouse session, which owns the bare terminal (the
    /// global input hooks + its stdout can't share ratatui's alternate screen).
    #[cfg(feature = "input-demo")]
    RunKbm(KbmRequest),
}

#[cfg(feature = "input-demo")]
#[derive(Clone)]
struct KbmRequest {
    /// true = controlled (receive input); false = controller (send input).
    controlled: bool,
    inject: bool,
    code: String,
    duration_s: u64,
}

/// Transport choices offered when hosting a test.
#[cfg(feature = "iroh")]
#[derive(Clone, Copy, PartialEq)]
enum XportMode {
    Tcp,
    Udp,
    #[cfg(feature = "ws")]
    Ws,
    Iroh,
}

#[cfg(feature = "iroh")]
impl XportMode {
    fn label(self) -> &'static str {
        match self {
            XportMode::Tcp => "TCP",
            XportMode::Udp => "UDP",
            #[cfg(feature = "ws")]
            XportMode::Ws => "WebSocket",
            XportMode::Iroh => "iroh / QUIC  (hole-punches NAT & firewalls)",
        }
    }
    fn hint(self) -> &'static str {
        match self {
            XportMode::Tcp | XportMode::Udp => "same LAN; advertises this host's IP",
            #[cfg(feature = "ws")]
            XportMode::Ws => "same LAN; HTTP-framed over TCP",
            XportMode::Iroh => "any network; only a code to share",
        }
    }
    /// True for the socket transports whose reachable `host:port` must be
    /// advertised (iroh instead shares a self-describing ticket).
    fn needs_host(self) -> bool {
        !matches!(self, XportMode::Iroh)
    }
    fn tag(self) -> &'static str {
        match self {
            XportMode::Tcp => "tcp",
            XportMode::Udp => "udp",
            #[cfg(feature = "ws")]
            XportMode::Ws => "ws",
            XportMode::Iroh => "iroh",
        }
    }
}

/// Cross-device + kbm UI state, compiled only when rendez-key is available.
#[cfg(feature = "iroh")]
struct Cross {
    // hosting
    host_modes: Vec<XportMode>,
    host_sel: usize,
    host_addr: String,
    host_code: Option<String>,
    host_addr_line: String,
    host_status: String,
    host_last: Option<String>,
    host_mbps: f64,
    host_stop: Option<oneshot::Sender<()>>,
    // joining
    code_input: String,
    reverse: bool,
    // kbm
    #[cfg(feature = "input-demo")]
    kbm_controlled: bool,
    #[cfg(feature = "input-demo")]
    kbm_inject: bool,
}

#[cfg(feature = "iroh")]
impl Cross {
    fn new() -> Self {
        #[allow(unused_mut)] // `mut` used only when the ws transport is pushed
        let mut host_modes = vec![XportMode::Iroh, XportMode::Tcp, XportMode::Udp];
        #[cfg(feature = "ws")]
        host_modes.push(XportMode::Ws);
        let host = netsu::p2p::addr::local_ipv4()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "127.0.0.1".to_string());
        Cross {
            host_modes,
            host_sel: 0,
            host_addr: format!("{host}:5201"),
            host_code: None,
            host_addr_line: String::new(),
            host_status: String::new(),
            host_last: None,
            host_mbps: 0.0,
            host_stop: None,
            code_input: String::new(),
            reverse: false,
            #[cfg(feature = "input-demo")]
            kbm_controlled: false,
            #[cfg(feature = "input-demo")]
            kbm_inject: false,
        }
    }
    fn host_mode(&self) -> XportMode {
        self.host_modes[self.host_sel.min(self.host_modes.len() - 1)]
    }
}

enum Screen {
    Home,
    Running,
    Summary,
    #[cfg(feature = "iroh")]
    HostConfig,
    #[cfg(feature = "iroh")]
    Hosting,
    #[cfg(feature = "iroh")]
    JoinConfig,
    #[cfg(feature = "input-demo")]
    KbmConfig,
}

struct App {
    screen: Screen,
    items: Vec<HomeItem>,
    menu: ListState,
    duration_s: u64,
    spinner: usize,
    rx: Option<UnboundedReceiver<UiMsg>>,
    rows: Vec<LiveRow>,
    spark: Vec<u64>,
    elapsed_ms: u64,
    running_title: String,
    summary: Summary,
    #[cfg(feature = "iroh")]
    cross: Cross,
    post: PostAction,
    quit: bool,
}

/// Entry point for `netsu tui`. Owns the terminal lifecycle; a kbm hand-off
/// restores the terminal, runs the session on it, then relaunches the menu.
pub async fn run() -> anyhow::Result<()> {
    // Loops only to relaunch the menu after a kbm hand-off; without input-demo
    // the single `Quit` arm returns on the first pass (hence the allow).
    #[allow(clippy::never_loop)]
    loop {
        let mut terminal = ratatui::init();
        let outcome = App::new().run(&mut terminal).await;
        ratatui::restore();
        match outcome? {
            PostAction::Quit => return Ok(()),
            #[cfg(feature = "input-demo")]
            PostAction::RunKbm(req) => {
                if let Err(e) = run_kbm(req).await {
                    eprintln!("netsu tui: kbm session: {e:#}");
                }
                // Loop back into the menu so the user can run another session.
            }
        }
    }
}

impl App {
    // Home items are pushed under feature gates; with iroh/input-demo off the
    // list collapses to a couple of unconditional pushes (hence the allow).
    #[allow(clippy::vec_init_then_push)]
    fn new() -> Self {
        let mut items: Vec<HomeItem> = Vec::new();
        #[cfg(feature = "iroh")]
        {
            items.push(HomeItem {
                label: "Host a speed test",
                hint: "pick a transport, share a code, serve joiners",
                act: Activity::HostTest,
            });
            items.push(HomeItem {
                label: "Join a speed test",
                hint: "type a host's code and measure the link",
                act: Activity::JoinTest,
            });
        }
        #[cfg(feature = "input-demo")]
        {
            items.push(HomeItem {
                label: "Keyboard/mouse — receive (controlled)",
                hint: "share a code; inject a controller's input",
                act: Activity::KbmControlled,
            });
            items.push(HomeItem {
                label: "Keyboard/mouse — send (controller)",
                hint: "type a code; stream this device's input",
                act: Activity::KbmController,
            });
        }
        items.push(HomeItem {
            label: "Local lab — upload throughput",
            hint: "loopback, single machine (offline)",
            act: Activity::LocalUpload,
        });
        items.push(HomeItem {
            label: "Local lab — reverse throughput",
            hint: "loopback download, single machine (offline)",
            act: Activity::LocalReverse,
        });
        #[cfg(feature = "iroh")]
        {
            items.push(HomeItem {
                label: "Local lab — mux: input under file load",
                hint: "does the high-priority probe stay low-latency?",
                act: Activity::LocalMuxInputFile,
            });
            items.push(HomeItem {
                label: "Local lab — mux: mixed workloads",
                hint: "input + clipboard + cast + file, graded priority",
                act: Activity::LocalMuxMixed,
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
            rx: None,
            rows: Vec::new(),
            spark: Vec::new(),
            elapsed_ms: 0,
            running_title: String::new(),
            summary: Summary::default(),
            #[cfg(feature = "iroh")]
            cross: Cross::new(),
            post: PostAction::Quit,
            quit: false,
        }
    }

    async fn run(&mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<PostAction> {
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
                msg = recv_msg(&mut self.rx), if self.rx.is_some() => {
                    match msg {
                        Some(m) => self.on_msg(m),
                        None => { self.rx = None; }
                    }
                }
            }
        }
        Ok(self.post.clone())
    }

    fn on_event(&mut self, ev: Event) {
        let Event::Key(key) = ev else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }
        match self.screen {
            Screen::Home => self.on_home_key(key.code),
            Screen::Running => {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    // Abort: drop the receiver; the task is detached and harmless.
                    self.rx = None;
                    self.screen = Screen::Home;
                }
            }
            Screen::Summary => match key.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Esc => self.screen = Screen::Home,
                KeyCode::Char('r') | KeyCode::Enter => self.restart(),
                _ => {}
            },
            #[cfg(feature = "iroh")]
            Screen::HostConfig => self.on_hostconfig_key(key.code),
            #[cfg(feature = "iroh")]
            Screen::Hosting => {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    self.stop_hosting();
                    self.screen = Screen::Home;
                }
            }
            #[cfg(feature = "iroh")]
            Screen::JoinConfig => self.on_joinconfig_key(key.code),
            #[cfg(feature = "input-demo")]
            Screen::KbmConfig => self.on_kbmconfig_key(key.code),
        }
    }

    fn on_home_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Right => {
                self.duration_s = (self.duration_s + 1).min(60);
            }
            KeyCode::Char('-') | KeyCode::Left => {
                self.duration_s = self.duration_s.saturating_sub(1).max(1);
            }
            KeyCode::Enter => self.choose(),
            _ => {}
        }
    }

    fn move_sel(&mut self, delta: i32) {
        let n = self.items.len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.menu.selected().unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(n);
        self.menu.select(Some(next as usize));
    }

    /// Act on the highlighted home row.
    fn choose(&mut self) {
        let idx = self.menu.selected().unwrap_or(0);
        let Some(item) = self.items.get(idx) else {
            return;
        };
        match item.act {
            Activity::LocalUpload => self.start_local(false),
            Activity::LocalReverse => self.start_local(true),
            #[cfg(feature = "iroh")]
            Activity::HostTest => self.screen = Screen::HostConfig,
            #[cfg(feature = "iroh")]
            Activity::JoinTest => {
                self.cross.code_input.clear();
                self.cross.reverse = false;
                self.screen = Screen::JoinConfig;
            }
            #[cfg(feature = "iroh")]
            Activity::LocalMuxInputFile => self.start_local_mux(true),
            #[cfg(feature = "iroh")]
            Activity::LocalMuxMixed => self.start_local_mux(false),
            #[cfg(feature = "input-demo")]
            Activity::KbmControlled => {
                self.cross.kbm_controlled = true;
                self.cross.kbm_inject = false;
                self.screen = Screen::KbmConfig;
            }
            #[cfg(feature = "input-demo")]
            Activity::KbmController => {
                self.cross.kbm_controlled = false;
                self.cross.code_input.clear();
                self.screen = Screen::KbmConfig;
            }
        }
    }

    /// Re-run whatever produced the current summary (the `r`/enter shortcut).
    fn restart(&mut self) {
        // Cheapest correct behavior: return to Home. Re-running a cross-device
        // client needs the peer still hosting, which we can't assume; a local
        // run is one keystroke away from Home anyway.
        self.screen = Screen::Home;
    }

    fn reset_live(&mut self, title: &str) {
        self.rows.clear();
        self.spark.clear();
        self.elapsed_ms = 0;
        self.running_title = title.to_string();
    }

    fn start_local(&mut self, reverse: bool) {
        let title = if reverse {
            "Local — reverse throughput"
        } else {
            "Local — upload throughput"
        };
        self.reset_live(title);
        self.screen = Screen::Running;
        let (tx, rx) = unbounded_channel();
        self.rx = Some(rx);
        spawn_local_throughput(reverse, self.duration_s, tx);
    }

    fn on_msg(&mut self, msg: UiMsg) {
        match msg {
            UiMsg::Live { elapsed_ms, rows } => {
                self.elapsed_ms = elapsed_ms;
                let peak = rows.iter().map(|r| r.mbps).fold(0.0, f64::max);
                self.push_spark(peak);
                self.rows = rows;
            }
            UiMsg::Done(summary) => {
                self.summary = summary;
                self.screen = Screen::Summary;
                self.rx = None;
            }
            #[cfg(feature = "iroh")]
            UiMsg::HostReady { code, addr_line } => {
                self.cross.host_code = code;
                self.cross.host_addr_line = addr_line;
                self.cross.host_status = "waiting for a peer to join…".into();
            }
            #[cfg(feature = "iroh")]
            UiMsg::HostFailed(e) => {
                self.summary = Summary {
                    title: "could not host".into(),
                    lines: vec![e],
                    cli: String::new(),
                    ok: false,
                };
                self.screen = Screen::Summary;
                self.rx = None;
                self.cross.host_stop = None;
            }
            #[cfg(feature = "iroh")]
            UiMsg::HostInterval { mbps } => {
                self.cross.host_mbps = mbps;
                self.cross.host_status = "peer connected — measuring…".into();
                self.push_spark(mbps);
            }
            #[cfg(feature = "iroh")]
            UiMsg::HostComplete { line } => {
                self.cross.host_mbps = 0.0;
                self.cross.host_last = Some(line);
                self.cross.host_status = "run complete — code still valid, waiting…".into();
            }
        }
    }

    fn push_spark(&mut self, mbps: f64) {
        self.spark.push(mbps.max(0.0) as u64);
        if self.spark.len() > 120 {
            self.spark.remove(0);
        }
    }

    // ---- rendering ----

    fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        f.render_widget(Block::default().style(Style::new().bg(T.base)), area);
        match self.screen {
            Screen::Home => self.render_home(f, area),
            Screen::Running => self.render_running(f, area),
            Screen::Summary => self.render_summary(f, area),
            #[cfg(feature = "iroh")]
            Screen::HostConfig => self.render_hostconfig(f, area),
            #[cfg(feature = "iroh")]
            Screen::Hosting => self.render_hosting(f, area),
            #[cfg(feature = "iroh")]
            Screen::JoinConfig => self.render_joinconfig(f, area),
            #[cfg(feature = "input-demo")]
            Screen::KbmConfig => self.render_kbmconfig(f, area),
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
                    "  cross-device speed + input latency",
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
                "choose an activity   (client duration {}s)",
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
                ("enter", "go"),
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

        f.render_widget(self.stream_table("streams"), table_area);
        f.render_widget(help_bar(&[("q/esc", "abort")]), help);
    }

    /// The per-stream table shared by the running + hosting dashboards.
    fn stream_table(&self, title: &str) -> Table<'static> {
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
        Table::new(
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
        .block(rounded(title))
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

        let title_color = if self.summary.ok { T.green } else { T.red };
        f.render_widget(
            Paragraph::new(Span::styled(
                &self.summary.title,
                Style::new().fg(title_color).bold(),
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
            help_bar(&[("r", "menu"), ("esc", "home"), ("q", "quit")]),
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

/// Await the optional message receiver (only polled when it is `Some`).
async fn recv_msg(rx: &mut Option<UnboundedReceiver<UiMsg>>) -> Option<UiMsg> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

fn spawn_local_throughput(reverse: bool, duration_s: u64, tx: UnboundedSender<UiMsg>) {
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
                let _ = tx.send(UiMsg::Done(Summary {
                    title: "server failed".into(),
                    lines: vec![e.to_string()],
                    cli: String::new(),
                    ok: false,
                }));
                return;
            }
        };
        let port = server.port;
        let tx_interval = tx.clone();
        let start = Instant::now();
        let on_interval = Box::new(move |r: netsu::stats::IntervalReport| {
            let _ = tx_interval.send(UiMsg::Live {
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
                ok: true,
            },
            Err(e) => Summary {
                title: "test failed".into(),
                lines: vec![e.to_string()],
                cli: String::new(),
                ok: false,
            },
        };
        let _ = tx.send(UiMsg::Done(summary));
    });
}

// ---------------------------------------------------------------------------
// Cross-device flow (host/join). Behind `iroh` because it publishes/claims
// rendez-key codes (rendez-key rides the reqwest that the iroh feature pulls in).
// ---------------------------------------------------------------------------

#[cfg(feature = "iroh")]
impl App {
    fn on_hostconfig_key(&mut self, code: KeyCode) {
        let n = self.cross.host_modes.len();
        match code {
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up => self.cross.host_sel = (self.cross.host_sel + n - 1) % n,
            KeyCode::Down => self.cross.host_sel = (self.cross.host_sel + 1) % n,
            KeyCode::Backspace if self.cross.host_mode().needs_host() => {
                self.cross.host_addr.pop();
            }
            KeyCode::Char(c)
                if self.cross.host_mode().needs_host()
                    && !c.is_whitespace()
                    && self.cross.host_addr.len() < 64 =>
            {
                self.cross.host_addr.push(c);
            }
            KeyCode::Enter => self.start_hosting(),
            _ => {}
        }
    }

    fn start_hosting(&mut self) {
        let mode = self.cross.host_mode();
        self.cross.host_code = None;
        self.cross.host_last = None;
        self.cross.host_mbps = 0.0;
        self.cross.host_addr_line = String::new();
        self.cross.host_status = "starting server…".into();
        self.spark.clear();
        self.rows.clear();
        let (tx, rx) = unbounded_channel();
        self.rx = Some(rx);
        let (stop_tx, stop_rx) = oneshot::channel();
        self.cross.host_stop = Some(stop_tx);
        self.screen = Screen::Hosting;
        spawn_host(mode, self.cross.host_addr.clone(), stop_rx, tx);
    }

    fn stop_hosting(&mut self) {
        if let Some(stop) = self.cross.host_stop.take() {
            let _ = stop.send(());
        }
        self.rx = None;
    }

    fn on_joinconfig_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Backspace => {
                self.cross.code_input.pop();
            }
            KeyCode::Tab => self.cross.reverse = !self.cross.reverse,
            KeyCode::Up => self.duration_s = (self.duration_s + 1).min(60),
            KeyCode::Down => self.duration_s = self.duration_s.saturating_sub(1).max(1),
            KeyCode::Char(c) if !c.is_whitespace() && self.cross.code_input.len() < 32 => {
                self.cross.code_input.push(c);
            }
            KeyCode::Enter => {
                let code = self.cross.code_input.trim().to_string();
                if code.is_empty() {
                    return;
                }
                self.reset_live("Join — connecting…");
                self.screen = Screen::Running;
                let (tx, rx) = unbounded_channel();
                self.rx = Some(rx);
                spawn_join(code, self.duration_s, self.cross.reverse, tx);
            }
            _ => {}
        }
    }

    fn start_local_mux(&mut self, input_file: bool) {
        let title = if input_file {
            "Local — mux: input under file load"
        } else {
            "Local — mux: mixed workloads"
        };
        self.reset_live(title);
        self.screen = Screen::Running;
        let (tx, rx) = unbounded_channel();
        self.rx = Some(rx);
        spawn_local_mux(input_file, self.duration_s, tx);
    }

    fn render_hostconfig(&self, f: &mut Frame, area: Rect) {
        let [title, list_area, addr_area, help] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .margin(1)
        .areas(area);

        f.render_widget(
            Paragraph::new(Span::styled(
                "pick how the peer reaches you",
                Style::new().fg(T.accent).bold(),
            ))
            .block(rounded("host a test")),
            title,
        );

        let items: Vec<ListItem> = self
            .cross
            .host_modes
            .iter()
            .map(|m| {
                ListItem::new(Line::from(vec![
                    Span::styled(m.label(), Style::new().fg(T.text)),
                    Span::raw("  "),
                    Span::styled(m.hint(), Style::new().fg(T.subtext).italic()),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(rounded("transport"))
            .highlight_style(
                Style::new()
                    .fg(T.base)
                    .bg(T.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        let mut state = ListState::default();
        state.select(Some(self.cross.host_sel));
        f.render_stateful_widget(list, list_area, &mut state);

        let addr_para = if self.cross.host_mode().needs_host() {
            Paragraph::new(Line::from(vec![
                Span::styled("advertise  ", Style::new().fg(T.subtext)),
                Span::styled(
                    format!("{}\u{2588}", self.cross.host_addr),
                    Style::new().fg(T.green),
                ),
            ]))
            .block(rounded("host:port  (editable — type to change)"))
        } else {
            Paragraph::new(Span::styled(
                "iroh generates a self-describing ticket — nothing to enter",
                Style::new().fg(T.subtext).italic(),
            ))
            .block(rounded("address"))
        };
        f.render_widget(addr_para, addr_area);

        f.render_widget(
            help_bar(&[
                ("↑/↓", "transport"),
                ("type", "edit host"),
                ("enter", "start"),
                ("esc", "back"),
            ]),
            help,
        );
    }

    fn render_hosting(&self, f: &mut Frame, area: Rect) {
        let [code_area, live_area, help] = Layout::vertical([
            Constraint::Length(7),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .margin(1)
        .areas(area);

        let spin = SPINNER[self.spinner % SPINNER.len()];
        let mut code_lines = Vec::new();
        match &self.cross.host_code {
            Some(code) => code_lines.push(Line::from(vec![
                Span::styled("code  ", Style::new().fg(T.subtext)),
                Span::styled(code.clone(), Style::new().fg(T.accent).bold()),
                Span::styled(
                    "   ← type this on the other device",
                    Style::new().fg(T.subtext).italic(),
                ),
            ])),
            None => code_lines.push(Line::from(Span::styled(
                format!("{spin} publishing a code…"),
                Style::new().fg(T.yellow),
            ))),
        }
        if !self.cross.host_addr_line.is_empty() {
            code_lines.push(Line::from(Span::styled(
                self.cross.host_addr_line.clone(),
                Style::new().fg(T.subtext),
            )));
        }
        code_lines.push(Line::from(Span::styled(
            format!("{spin} {}", self.cross.host_status),
            Style::new().fg(T.green),
        )));
        f.render_widget(
            Paragraph::new(code_lines).block(rounded("hosting — share the code")),
            code_area,
        );

        let [spark, foot] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(4)]).areas(live_area);
        f.render_widget(
            Sparkline::default()
                .block(rounded("server-side throughput (Mbps)"))
                .data(&self.spark)
                .style(Style::new().fg(T.blue)),
            spark,
        );
        let mut foot_lines = vec![Line::from(vec![
            Span::styled("current  ", Style::new().fg(T.subtext)),
            Span::styled(
                format!("{:.1} Mbit/s", self.cross.host_mbps),
                Style::new().fg(T.blue).bold(),
            ),
        ])];
        if let Some(last) = &self.cross.host_last {
            foot_lines.push(Line::from(Span::styled(
                last.clone(),
                Style::new().fg(T.text),
            )));
        }
        f.render_widget(Paragraph::new(foot_lines).block(rounded("live")), foot);

        f.render_widget(help_bar(&[("q/esc", "stop hosting")]), help);
    }

    fn render_joinconfig(&self, f: &mut Frame, area: Rect) {
        let [title, input_area, opts_area, help] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .margin(1)
        .areas(area);

        f.render_widget(
            Paragraph::new(Span::styled(
                "enter the host's code",
                Style::new().fg(T.accent).bold(),
            ))
            .block(rounded("join a test")),
            title,
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("code  ", Style::new().fg(T.subtext)),
                Span::styled(
                    format!("{}\u{2588}", self.cross.code_input),
                    Style::new().fg(T.green).bold(),
                ),
            ]))
            .block(rounded(
                "code  (type it; the transport comes from the code)",
            )),
            input_area,
        );

        let rev = if self.cross.reverse {
            "on (server sends)"
        } else {
            "off (client sends)"
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("duration {}s", self.duration_s),
                    Style::new().fg(T.text),
                ),
                Span::raw("      "),
                Span::styled(format!("reverse {rev}"), Style::new().fg(T.peach)),
            ]))
            .block(rounded("options")),
            opts_area,
        );

        f.render_widget(
            help_bar(&[
                ("type", "code"),
                ("↑/↓", "duration"),
                ("tab", "reverse"),
                ("enter", "join"),
                ("esc", "back"),
            ]),
            help,
        );
    }
}

#[cfg(feature = "iroh")]
fn err_summary(title: &str, detail: &str) -> Summary {
    Summary {
        title: title.into(),
        lines: vec![detail.into()],
        cli: String::new(),
        ok: false,
    }
}

#[cfg(feature = "iroh")]
fn cli_hint(tag: &str, code: &str, dur: u64, reverse: bool) -> String {
    let r = if reverse { " -R" } else { "" };
    match tag {
        "iroh" => format!("netsu client {code} --iroh -t {dur}{r}"),
        "udp" => format!("netsu client <host> -u -t {dur}{r}"),
        "ws" => format!("netsu client <host> --ws -t {dur}{r}"),
        _ => format!("netsu client <host> -t {dur}{r}"),
    }
}

#[cfg(feature = "iroh")]
fn spawn_host(
    mode: XportMode,
    host_addr: String,
    stop_rx: oneshot::Receiver<()>,
    tx: UnboundedSender<UiMsg>,
) {
    use netsu::client::Transport;
    use netsu::p2p::{addr, rendezkey};
    use netsu::server::{ServerEvent, ServerOptions, ServerReporter, start_server};

    tokio::spawn(async move {
        // UDP data rides a TCP control channel, so tcp/udp share one server.
        let transport = match mode {
            XportMode::Tcp | XportMode::Udp => Transport::Tcp,
            #[cfg(feature = "ws")]
            XportMode::Ws => Transport::Ws,
            XportMode::Iroh => Transport::Iroh,
        };
        let bind_port = if mode.needs_host() {
            host_addr
                .rsplit_once(':')
                .and_then(|(_, p)| p.parse().ok())
                .unwrap_or(5201)
        } else {
            0
        };

        let tx_ev = tx.clone();
        let reporter: ServerReporter = std::sync::Arc::new(move |ev| match ev {
            ServerEvent::Interval(r) => {
                let _ = tx_ev.send(UiMsg::HostInterval {
                    mbps: r.bits_per_second / 1e6,
                });
            }
            ServerEvent::Complete {
                duration_seconds,
                bytes,
                bits_per_second,
            } => {
                let _ = tx_ev.send(UiMsg::HostComplete {
                    line: format!(
                        "last run: {:.1} Mbit/s over {:.1}s ({} bytes)",
                        bits_per_second / 1e6,
                        duration_seconds,
                        bytes
                    ),
                });
            }
        });

        let server = match start_server(ServerOptions {
            port: bind_port,
            transport,
            on_event: Some(reporter),
            ..Default::default()
        })
        .await
        {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(UiMsg::HostFailed(format!("{e}")));
                return;
            }
        };

        // What a joiner needs to reach us: an iroh ticket, or host:bound-port.
        let addr_value = match mode {
            XportMode::Iroh => server.endpoint_ticket.clone().unwrap_or_default(),
            _ => {
                let host = host_addr
                    .rsplit_once(':')
                    .map(|(h, _)| h.to_string())
                    .unwrap_or_else(|| host_addr.clone());
                format!("{host}:{}", server.port)
            }
        };
        let blob = addr::encode_rendezvous(mode.tag(), &addr_value);
        let token = rendezkey::token_from_env();
        let code = rendezkey::store(
            rendezkey::DEFAULT_BASE_URL,
            token.as_deref(),
            &blob,
            rendezkey::ANON_MAX_TTL_SECS,
            rendezkey::ANON_MAX_READS,
        )
        .await
        .ok();
        let addr_line = match (&code, mode) {
            (Some(_), XportMode::Iroh) => {
                "iroh ticket — reachable across NAT/firewalls".to_string()
            }
            (Some(_), _) => format!("dial {addr_value} on your LAN"),
            (None, _) => format!("rendez-key unavailable — share manually: {addr_value}"),
        };
        let _ = tx.send(UiMsg::HostReady { code, addr_line });

        // Hold the port open until the UI asks us to stop; the accept loop
        // keeps serving joiners (the code is good for several claims).
        let _ = stop_rx.await;
        server.close().await;
    });
}

#[cfg(feature = "iroh")]
fn spawn_join(code: String, duration_s: u64, reverse: bool, tx: UnboundedSender<UiMsg>) {
    use netsu::client::{ClientOptions, Transport, run_client};
    use netsu::p2p::{addr, rendezkey};

    tokio::spawn(async move {
        let blob = match rendezkey::claim(rendezkey::DEFAULT_BASE_URL, &code).await {
            Ok(b) => b,
            Err(e) => {
                let _ = tx.send(UiMsg::Done(err_summary(
                    "could not claim code",
                    &format!("{e:#}"),
                )));
                return;
            }
        };
        let (tag, addr_str) = addr::decode_rendezvous(&blob);
        let (transport, udp) = match tag.as_str() {
            "tcp" => (Transport::Tcp, false),
            "udp" => (Transport::Tcp, true),
            "ws" => {
                #[cfg(feature = "ws")]
                {
                    (Transport::Ws, false)
                }
                #[cfg(not(feature = "ws"))]
                {
                    let _ = tx.send(UiMsg::Done(err_summary(
                        "unsupported transport",
                        "the host chose WebSocket, but this build lacks --features ws",
                    )));
                    return;
                }
            }
            "iroh" => (Transport::Iroh, false),
            other => {
                let _ = tx.send(UiMsg::Done(err_summary(
                    "unknown transport",
                    &format!("the code carried an unknown transport '{other}'"),
                )));
                return;
            }
        };
        // iroh's addr is a ticket (host arg); sockets carry host:port.
        let (host, port) = if tag == "iroh" {
            (addr_str.clone(), 0u16)
        } else {
            match addr_str.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse().unwrap_or(5201)),
                None => (addr_str.clone(), 5201),
            }
        };

        let tx_i = tx.clone();
        let start = Instant::now();
        let label = tag.to_uppercase();
        let on_interval = Box::new(move |r: netsu::stats::IntervalReport| {
            let _ = tx_i.send(UiMsg::Live {
                elapsed_ms: start.elapsed().as_millis() as u64,
                rows: vec![LiveRow {
                    label: format!("{label} throughput"),
                    priority: None,
                    mbps: r.bits_per_second / 1e6,
                    measured: false,
                }],
            });
        });
        let opts = ClientOptions {
            port,
            transport,
            udp,
            reverse,
            duration: duration_s as u32,
            ..Default::default()
        };
        let summary = match run_client(&host, opts, Some(on_interval)).await {
            Ok(r) => Summary {
                title: "test complete".into(),
                lines: vec![
                    format!("transport {tag}"),
                    format!("sent      {:.1} Mbit/s", r.send_bits_per_second / 1e6),
                    format!("received  {:.1} Mbit/s", r.receive_bits_per_second / 1e6),
                    format!(
                        "bytes     {} sent / {} received",
                        r.sent_bytes, r.received_bytes
                    ),
                ],
                cli: cli_hint(&tag, &code, duration_s, reverse),
                ok: true,
            },
            Err(e) => err_summary("test failed", &e.to_string()),
        };
        let _ = tx.send(UiMsg::Done(summary));
    });
}

#[cfg(feature = "iroh")]
fn spawn_local_mux(input_file: bool, duration_s: u64, tx: UnboundedSender<UiMsg>) {
    use netsu::mux::config::{RunConfig, ScenarioName};
    use netsu::mux::protocol::MUX_ALPN;
    use netsu::mux::runner::{LiveSnapshot, run_with_live};
    use netsu::mux::{receiver, result::MuxResult};
    use netsu::p2p::endpoint::LocalPair;

    let (scenario, scenario_cli) = if input_file {
        (ScenarioName::InputFile, "input-file")
    } else {
        (ScenarioName::Mixed, "mixed")
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
                let _ = tx.send(UiMsg::Done(err_summary(
                    "iroh setup failed",
                    &format!("{e:#}"),
                )));
                return;
            }
        };
        let server_conn = pair.server_connection.clone();
        let serve = tokio::spawn(async move { receiver::serve(server_conn).await });

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
                let _ = tx_fwd.send(UiMsg::Live {
                    elapsed_ms: snap.elapsed_ms,
                    rows,
                });
            }
        });

        let outcome = run_with_live(&pair.client_connection, &config, Some(live_tx)).await;
        let _ = serve.await;
        let _ = fwd.await;
        pair.close().await;

        let summary = match outcome {
            Ok(o) => {
                let result = MuxResult::from_outcome(&o, config.seed);
                let mut lines = Vec::new();
                for s in &result.streams {
                    match &s.latency {
                        Some(l) => lines.push(format!(
                            "{:<12} prio {:>2}  {:>7.1} Mbps  p50 {:.2}ms p99 {:.2}ms  miss {:.1}%",
                            s.kind,
                            s.priority,
                            s.throughput_mbps,
                            l.p50_us as f64 / 1000.0,
                            l.p99_us as f64 / 1000.0,
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
                    ok: true,
                }
            }
            Err(e) => err_summary("mux run failed", &format!("{e:#}")),
        };
        let _ = tx.send(UiMsg::Done(summary));
    });
}

// ---------------------------------------------------------------------------
// Keyboard/mouse sharing (input-demo). Collected in the TUI, then handed off to
// the bare terminal — global input capture and its stdout can't share the
// alternate screen ratatui owns.
// ---------------------------------------------------------------------------

#[cfg(feature = "input-demo")]
impl App {
    fn on_kbmconfig_key(&mut self, code: KeyCode) {
        if matches!(code, KeyCode::Esc) {
            self.screen = Screen::Home;
            return;
        }
        if self.cross.kbm_controlled {
            match code {
                KeyCode::Char('i') => self.cross.kbm_inject = !self.cross.kbm_inject,
                KeyCode::Enter => {
                    self.post = PostAction::RunKbm(KbmRequest {
                        controlled: true,
                        inject: self.cross.kbm_inject,
                        code: String::new(),
                        duration_s: 0,
                    });
                    self.quit = true;
                }
                _ => {}
            }
        } else {
            match code {
                KeyCode::Backspace => {
                    self.cross.code_input.pop();
                }
                KeyCode::Up => self.duration_s = (self.duration_s + 1).min(60),
                KeyCode::Down => self.duration_s = self.duration_s.saturating_sub(1).max(1),
                KeyCode::Char(c) if !c.is_whitespace() && self.cross.code_input.len() < 32 => {
                    self.cross.code_input.push(c);
                }
                KeyCode::Enter => {
                    let code = self.cross.code_input.trim().to_string();
                    if code.is_empty() {
                        return;
                    }
                    self.post = PostAction::RunKbm(KbmRequest {
                        controlled: false,
                        inject: false,
                        code,
                        duration_s: self.duration_s,
                    });
                    self.quit = true;
                }
                _ => {}
            }
        }
    }

    fn render_kbmconfig(&self, f: &mut Frame, area: Rect) {
        let [title, body, help] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .margin(1)
        .areas(area);

        if self.cross.kbm_controlled {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "keyboard/mouse — receive (controlled)",
                    Style::new().fg(T.accent).bold(),
                ))
                .block(rounded("kbm receive")),
                title,
            );
            let (inject, inj_color) = if self.cross.kbm_inject {
                (
                    "ENABLED — received input moves your real cursor/keys",
                    T.red,
                )
            } else {
                (
                    "OFF — received input is measured but NOT injected",
                    T.subtext,
                )
            };
            let lines = vec![
                Line::from(Span::styled(
                    "On start, a rendez-key code is shown; the controller types it.",
                    Style::new().fg(T.text),
                )),
                Line::from(Span::raw("")),
                Line::from(vec![
                    Span::styled("injection  ", Style::new().fg(T.subtext)),
                    Span::styled(inject, Style::new().fg(inj_color).bold()),
                ]),
                Line::from(Span::raw("")),
                Line::from(Span::styled(
                    "The session takes over the terminal (global capture can't share",
                    Style::new().fg(T.subtext).italic(),
                )),
                Line::from(Span::styled(
                    "the TUI screen); press q there to stop, then you return here.",
                    Style::new().fg(T.subtext).italic(),
                )),
            ];
            f.render_widget(Paragraph::new(lines).block(rounded("controlled")), body);
            f.render_widget(
                help_bar(&[
                    ("i", "toggle injection"),
                    ("enter", "start"),
                    ("esc", "back"),
                ]),
                help,
            );
        } else {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "keyboard/mouse — send (controller)",
                    Style::new().fg(T.accent).bold(),
                ))
                .block(rounded("kbm send")),
                title,
            );
            let lines = vec![
                Line::from(vec![
                    Span::styled("code  ", Style::new().fg(T.subtext)),
                    Span::styled(
                        format!("{}\u{2588}", self.cross.code_input),
                        Style::new().fg(T.green).bold(),
                    ),
                ]),
                Line::from(Span::raw("")),
                Line::from(Span::styled(
                    format!(
                        "duration {}s — stream this device's input to the peer",
                        self.duration_s
                    ),
                    Style::new().fg(T.text),
                )),
                Line::from(Span::raw("")),
                Line::from(Span::styled(
                    "On start, this device's keyboard & mouse are captured globally",
                    Style::new().fg(T.subtext).italic(),
                )),
                Line::from(Span::styled(
                    "and streamed; press q or Esc+Ctrl+Alt to stop.",
                    Style::new().fg(T.subtext).italic(),
                )),
            ];
            f.render_widget(Paragraph::new(lines).block(rounded("controller")), body);
            f.render_widget(
                help_bar(&[
                    ("type", "code"),
                    ("↑/↓", "duration"),
                    ("enter", "start"),
                    ("esc", "back"),
                ]),
                help,
            );
        }
    }
}

#[cfg(feature = "input-demo")]
async fn run_kbm(req: KbmRequest) -> anyhow::Result<()> {
    use netsu::demo::session::{
        ControlledConfig, ControllerConfig, run_controlled, run_controller,
    };
    if req.controlled {
        println!("netsu tui → keyboard/mouse: receiving (controlled)");
        run_controlled(ControlledConfig {
            allow_peer: None,
            inject_input: req.inject,
            idle_timeout: Duration::from_secs(3),
            direct_only: false,
            no_rendezkey: false,
            rendezkey_url: None,
        })
        .await
    } else {
        println!("netsu tui → keyboard/mouse: sending (controller)");
        run_controller(ControllerConfig {
            peer: req.code,
            duration: Duration::from_secs(req.duration_s),
            bulk_streams: 1,
            bulk_rate_mbps: None,
            hook_capacity: 4096,
            direct_only: false,
            no_rendezkey: false,
            rendezkey_url: None,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn rendered(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(96, 28)).unwrap();
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
        assert!(screen.contains("Local lab"));
        assert!(screen.contains("choose an activity"));
    }

    #[cfg(feature = "iroh")]
    #[test]
    fn home_offers_cross_device_when_iroh_enabled() {
        let mut app = App::new();
        let screen = rendered(&mut app);
        assert!(screen.contains("Host a speed test"));
        assert!(screen.contains("Join a speed test"));
    }

    #[cfg(feature = "iroh")]
    #[test]
    fn hostconfig_lists_transports() {
        let mut app = App::new();
        app.screen = Screen::HostConfig;
        let screen = rendered(&mut app);
        assert!(screen.contains("TCP"));
        assert!(screen.contains("iroh"));
        assert!(screen.contains("host a test"));
    }

    #[cfg(feature = "iroh")]
    #[test]
    fn joinconfig_prompts_for_a_code() {
        let mut app = App::new();
        app.screen = Screen::JoinConfig;
        app.cross.code_input = "7K3MQ9TX".into();
        let screen = rendered(&mut app);
        assert!(screen.contains("code"));
        assert!(screen.contains("7K3MQ9TX"));
    }

    #[cfg(feature = "iroh")]
    #[test]
    fn hosting_shows_the_code() {
        let mut app = App::new();
        app.screen = Screen::Hosting;
        app.cross.host_code = Some("7K3MQ9TX".into());
        app.cross.host_addr_line = "iroh ticket published".into();
        app.cross.host_status = "waiting for a peer to join…".into();
        let screen = rendered(&mut app);
        assert!(screen.contains("7K3MQ9TX"));
        assert!(screen.contains("waiting for a peer"));
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
            ok: true,
        };
        let screen = rendered(&mut app);
        assert!(screen.contains("mux run complete"));
        assert!(screen.contains("p99 1.80ms"));
        assert!(screen.contains("netsu mux local"));
    }
}
