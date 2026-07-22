//! `netsu tui`: a ratatui launcher + live dashboard whose reason to exist is
//! **cross-device** testing without memorizing flags. You pick a role and a
//! transport, and both ends show a live speed log. How the joiner reaches the
//! host depends on the transport: socket transports (TCP/UDP/WebSocket) work
//! like iperf3 — the host advertises its `host:port` and the joiner dials it
//! directly. Native QUIC also dials `host:port`; iroh publishes a short
//! rendez-key code for its self-describing ticket; WebRTC exchanges a room code
//! through the signaling service and then requires a direct peer path. Testing
//! against yourself on one machine proves
//! nothing, so the headline flow connects two machines — the local loopback
//! runs are kept only as an offline "lab".
//!
//! A `--features tui`-only build keeps just the loopback lab. Cross-device
//! screens appear when at least one of `iroh`, `quic`, or `webrtc` is enabled.
//! The keyboard/mouse screens additionally need `--features input-demo`.

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
#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
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

#[cfg(feature = "webrtc")]
const DEFAULT_SIGNAL_URL: &str = "https://rendez-key.xc.huakun.tech/v1/signal";
#[cfg(feature = "webrtc")]
const DEFAULT_STUN_URLS: &str = "stun:stun.cloudflare.com:3478";

#[cfg(feature = "webrtc")]
fn webrtc_defaults_with(lookup: impl Fn(&str) -> Option<String>) -> (String, String) {
    let signal_url = lookup("NETSU_SIGNAL_URL").unwrap_or_else(|| DEFAULT_SIGNAL_URL.to_string());
    let stun_urls = lookup("NETSU_STUN_URLS").unwrap_or_else(|| DEFAULT_STUN_URLS.to_string());
    (signal_url, stun_urls)
}

#[cfg(feature = "webrtc")]
fn parse_stun_urls_field(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Which activity a home-menu row launches.
#[derive(Clone, Copy, PartialEq)]
enum Activity {
    /// Host a speed test — pick a transport, publish a code, serve joiners.
    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
    HostTest,
    /// Join a speed test — type a host's code and run against it.
    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
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
    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
    HostReady {
        code: Option<String>,
        addr_line: String,
    },
    /// The hosted server could not start.
    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
    HostFailed(String),
    /// A peer's test pushed one interval of server-side throughput.
    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
    HostInterval { mbps: f64 },
    /// A peer's test completed; the host keeps listening for the next one.
    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
    HostComplete { line: String },
    /// A one-shot WebRTC room reached its terminal outcome and is consumed.
    #[cfg(feature = "webrtc")]
    HostFinished,
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
#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
#[derive(Clone, Copy, PartialEq)]
enum XportMode {
    Tcp,
    Udp,
    #[cfg(feature = "ws")]
    Ws,
    #[cfg(feature = "iroh")]
    Iroh,
    #[cfg(feature = "quic")]
    Quic,
    #[cfg(feature = "webrtc")]
    WebRtc,
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
impl XportMode {
    fn label(self) -> &'static str {
        match self {
            XportMode::Tcp => "TCP",
            XportMode::Udp => "UDP",
            #[cfg(feature = "ws")]
            XportMode::Ws => "WebSocket",
            #[cfg(feature = "iroh")]
            XportMode::Iroh => "iroh / QUIC  (hole-punches NAT & firewalls)",
            #[cfg(feature = "quic")]
            XportMode::Quic => "Native QUIC",
            #[cfg(feature = "webrtc")]
            XportMode::WebRtc => "WebRTC",
        }
    }
    fn hint(self) -> &'static str {
        match self {
            XportMode::Tcp | XportMode::Udp => "same LAN; advertises this host's IP",
            #[cfg(feature = "ws")]
            XportMode::Ws => "same LAN; HTTP-framed over TCP",
            #[cfg(feature = "iroh")]
            XportMode::Iroh => "any network; only a code to share",
            #[cfg(feature = "quic")]
            XportMode::Quic => "fixed address; benchmark TLS",
            #[cfg(feature = "webrtc")]
            XportMode::WebRtc => "direct only; signaling + STUN",
        }
    }
    /// True for the socket transports whose reachable `host:port` must be
    /// advertised (iroh instead shares a self-describing ticket).
    fn needs_host(self) -> bool {
        match self {
            #[cfg(feature = "iroh")]
            XportMode::Iroh => false,
            #[cfg(feature = "webrtc")]
            XportMode::WebRtc => false,
            _ => true,
        }
    }
    fn tag(self) -> &'static str {
        match self {
            XportMode::Tcp => "tcp",
            XportMode::Udp => "udp",
            #[cfg(feature = "ws")]
            XportMode::Ws => "ws",
            #[cfg(feature = "iroh")]
            XportMode::Iroh => "iroh",
            #[cfg(feature = "quic")]
            XportMode::Quic => "quic",
            #[cfg(feature = "webrtc")]
            XportMode::WebRtc => "webrtc",
        }
    }
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostFocus {
    Transport,
    Address,
    #[cfg(feature = "webrtc")]
    Signal,
    #[cfg(feature = "webrtc")]
    Stun,
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JoinFocus {
    Transport,
    Target,
    #[cfg(feature = "webrtc")]
    Signal,
    #[cfg(feature = "webrtc")]
    Stun,
    Options,
}

/// Cross-device + kbm UI state, compiled only when rendez-key is available.
#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
struct Cross {
    // hosting
    host_modes: Vec<XportMode>,
    host_sel: usize,
    host_focus: HostFocus,
    host_addr: String,
    host_code: Option<String>,
    host_addr_line: String,
    host_status: String,
    host_last: Option<String>,
    host_mbps: f64,
    host_stop: Option<oneshot::Sender<()>>,
    // joining
    join_sel: usize,
    join_focus: JoinFocus,
    /// Holds the host's rendez-key code (iroh) or a `host:port` (sockets).
    code_input: String,
    reverse: bool,
    #[cfg(feature = "webrtc")]
    signal_url: String,
    #[cfg(feature = "webrtc")]
    stun_urls: String,
    form_error: String,
    // kbm
    #[cfg(feature = "input-demo")]
    kbm_controlled: bool,
    #[cfg(feature = "input-demo")]
    kbm_inject: bool,
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
impl Cross {
    fn new() -> Self {
        #[allow(unused_mut)] // `mut` used only when the ws transport is pushed
        let mut host_modes = Vec::new();
        #[cfg(feature = "iroh")]
        host_modes.push(XportMode::Iroh);
        #[cfg(feature = "webrtc")]
        host_modes.push(XportMode::WebRtc);
        #[cfg(feature = "quic")]
        host_modes.push(XportMode::Quic);
        host_modes.extend([XportMode::Tcp, XportMode::Udp]);
        #[cfg(feature = "ws")]
        host_modes.push(XportMode::Ws);
        let host = default_advertise_host_with(detect_local_ipv4);
        #[cfg(feature = "webrtc")]
        let (signal_url, stun_urls) = webrtc_defaults_with(|name| std::env::var(name).ok());
        Cross {
            host_modes,
            host_sel: 0,
            host_focus: HostFocus::Transport,
            host_addr: format!("{host}:5201"),
            host_code: None,
            host_addr_line: String::new(),
            host_status: String::new(),
            host_last: None,
            host_mbps: 0.0,
            host_stop: None,
            join_sel: 0,
            join_focus: JoinFocus::Transport,
            code_input: String::new(),
            reverse: false,
            #[cfg(feature = "webrtc")]
            signal_url,
            #[cfg(feature = "webrtc")]
            stun_urls,
            form_error: String::new(),
            #[cfg(feature = "input-demo")]
            kbm_controlled: false,
            #[cfg(feature = "input-demo")]
            kbm_inject: false,
        }
    }
    fn host_mode(&self) -> XportMode {
        self.host_modes[self.host_sel.min(self.host_modes.len() - 1)]
    }
    /// The transport the joiner picked (shares the same list as hosting).
    fn join_mode(&self) -> XportMode {
        self.host_modes[self.join_sel.min(self.host_modes.len() - 1)]
    }

    fn transport_config(&self, mode: XportMode) -> Result<CrossTransportConfig, String> {
        #[allow(unused_mut)]
        let mut config = CrossTransportConfig::default();
        #[cfg(not(feature = "webrtc"))]
        let _ = mode;
        #[cfg(feature = "webrtc")]
        if matches!(mode, XportMode::WebRtc) {
            config.webrtc = Some(build_tui_webrtc_options(&self.signal_url, &self.stun_urls)?);
        }
        Ok(config)
    }

    fn next_host_focus(&mut self, backwards: bool) {
        let fields = match self.host_mode() {
            #[cfg(feature = "webrtc")]
            XportMode::WebRtc => vec![HostFocus::Transport, HostFocus::Signal, HostFocus::Stun],
            mode if mode.needs_host() => vec![HostFocus::Transport, HostFocus::Address],
            _ => vec![HostFocus::Transport],
        };
        let current = fields
            .iter()
            .position(|focus| *focus == self.host_focus)
            .unwrap_or(0);
        let next = if backwards {
            (current + fields.len() - 1) % fields.len()
        } else {
            (current + 1) % fields.len()
        };
        self.host_focus = fields[next];
    }

    fn next_join_focus(&mut self, backwards: bool) {
        let fields = match self.join_mode() {
            #[cfg(feature = "webrtc")]
            XportMode::WebRtc => vec![
                JoinFocus::Transport,
                JoinFocus::Target,
                JoinFocus::Signal,
                JoinFocus::Stun,
                JoinFocus::Options,
            ],
            _ => vec![JoinFocus::Transport, JoinFocus::Target, JoinFocus::Options],
        };
        let current = fields
            .iter()
            .position(|focus| *focus == self.join_focus)
            .unwrap_or(0);
        let next = if backwards {
            (current + fields.len() - 1) % fields.len()
        } else {
            (current + 1) % fields.len()
        };
        self.join_focus = fields[next];
    }
}

enum Screen {
    Home,
    Running,
    Summary,
    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
    HostConfig,
    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
    Hosting,
    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
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
    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
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
        #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
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
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
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
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
            Screen::HostConfig => self.on_hostconfig_key(key.code),
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
            Screen::Hosting => {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    self.stop_hosting();
                    self.screen = Screen::Home;
                }
            }
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
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
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
            Activity::HostTest => {
                self.cross.host_focus = HostFocus::Transport;
                self.cross.form_error.clear();
                self.screen = Screen::HostConfig;
            }
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
            Activity::JoinTest => {
                self.cross.code_input.clear();
                self.cross.join_sel = 0;
                self.cross.join_focus = JoinFocus::Transport;
                self.cross.reverse = false;
                self.cross.form_error.clear();
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
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
            UiMsg::HostReady { code, addr_line } => {
                self.cross.host_code = code;
                self.cross.host_addr_line = addr_line;
                self.cross.host_status = "waiting for a peer to join…".into();
            }
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
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
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
            UiMsg::HostInterval { mbps } => {
                self.cross.host_mbps = mbps;
                self.cross.host_status = "peer connected — measuring…".into();
                self.push_spark(mbps);
            }
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
            UiMsg::HostComplete { line } => {
                self.cross.host_mbps = 0.0;
                self.cross.host_last = Some(line);
                #[cfg(feature = "webrtc")]
                if matches!(self.cross.host_mode(), XportMode::WebRtc) {
                    self.cross.host_status = "run complete — finishing one-shot room…".into();
                } else {
                    self.cross.host_status = "run complete — code still valid, waiting…".into();
                }
                #[cfg(not(feature = "webrtc"))]
                {
                    self.cross.host_status = "run complete — code still valid, waiting…".into();
                }
            }
            #[cfg(feature = "webrtc")]
            UiMsg::HostFinished => {
                self.cross.host_mbps = 0.0;
                self.cross.host_status =
                    "run complete — room consumed; press Esc and host again".into();
                self.cross.host_stop = None;
                self.rx = None;
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
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
            Screen::HostConfig => self.render_hostconfig(f, area),
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
            Screen::Hosting => self.render_hosting(f, area),
            #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
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

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn form_block(title: &str, focused: bool) -> Block<'static> {
    let mut block = rounded(title);
    if focused {
        block = block
            .border_type(BorderType::Double)
            .border_style(Style::new().fg(T.accent))
            .title(Span::styled(
                format!(" {title} "),
                Style::new().fg(T.accent).bold(),
            ));
    }
    block
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
// Cross-device flow (host/join). Available when any cross-device transport is
// compiled; only iroh uses rendez-key indirection.
// ---------------------------------------------------------------------------

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
impl App {
    fn on_hostconfig_key(&mut self, code: KeyCode) {
        let n = self.cross.host_modes.len();
        match code {
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Tab => self.cross.next_host_focus(false),
            KeyCode::BackTab => self.cross.next_host_focus(true),
            KeyCode::Up if self.cross.host_focus == HostFocus::Transport => {
                self.cross.host_sel = (self.cross.host_sel + n - 1) % n;
                self.cross.form_error.clear();
            }
            KeyCode::Down if self.cross.host_focus == HostFocus::Transport => {
                self.cross.host_sel = (self.cross.host_sel + 1) % n;
                self.cross.form_error.clear();
            }
            KeyCode::Backspace if self.cross.host_focus == HostFocus::Address => {
                self.cross.host_addr.pop();
                self.cross.form_error.clear();
            }
            KeyCode::Char(c)
                if self.cross.host_focus == HostFocus::Address
                    && !c.is_whitespace()
                    && self.cross.host_addr.len() < 255 =>
            {
                self.cross.host_addr.push(c);
                self.cross.form_error.clear();
            }
            #[cfg(feature = "webrtc")]
            KeyCode::Backspace if self.cross.host_focus == HostFocus::Signal => {
                self.cross.signal_url.pop();
                self.cross.form_error.clear();
            }
            #[cfg(feature = "webrtc")]
            KeyCode::Char(c)
                if self.cross.host_focus == HostFocus::Signal
                    && !c.is_whitespace()
                    && self.cross.signal_url.len() < 1024 =>
            {
                self.cross.signal_url.push(c);
                self.cross.form_error.clear();
            }
            #[cfg(feature = "webrtc")]
            KeyCode::Backspace if self.cross.host_focus == HostFocus::Stun => {
                self.cross.stun_urls.pop();
                self.cross.form_error.clear();
            }
            #[cfg(feature = "webrtc")]
            KeyCode::Char(c)
                if self.cross.host_focus == HostFocus::Stun
                    && !c.is_whitespace()
                    && self.cross.stun_urls.len() < 2048 =>
            {
                self.cross.stun_urls.push(c);
                self.cross.form_error.clear();
            }
            KeyCode::Enter => self.start_hosting(),
            _ => {}
        }
    }

    fn start_hosting(&mut self) {
        let mode = self.cross.host_mode();
        if mode.needs_host()
            && let Err(error) = parse_host_port(&self.cross.host_addr)
        {
            self.cross.form_error = error;
            return;
        }
        let transport_config = match self.cross.transport_config(mode) {
            Ok(config) => config,
            Err(error) => {
                self.cross.form_error = error;
                return;
            }
        };
        self.cross.form_error.clear();
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
        spawn_host(
            mode,
            self.cross.host_addr.clone(),
            transport_config,
            stop_rx,
            tx,
        );
    }

    fn stop_hosting(&mut self) {
        if let Some(stop) = self.cross.host_stop.take() {
            let _ = stop.send(());
        }
        self.rx = None;
    }

    fn on_joinconfig_key(&mut self, code: KeyCode) {
        let n = self.cross.host_modes.len();
        match code {
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Tab => self.cross.next_join_focus(false),
            KeyCode::BackTab => self.cross.next_join_focus(true),
            KeyCode::Up if self.cross.join_focus == JoinFocus::Transport => {
                self.cross.join_sel = (self.cross.join_sel + n - 1) % n;
                self.cross.form_error.clear();
            }
            KeyCode::Down if self.cross.join_focus == JoinFocus::Transport => {
                self.cross.join_sel = (self.cross.join_sel + 1) % n;
                self.cross.form_error.clear();
            }
            KeyCode::Backspace if self.cross.join_focus == JoinFocus::Target => {
                self.cross.code_input.pop();
                self.cross.form_error.clear();
            }
            KeyCode::Right if self.cross.join_focus == JoinFocus::Options => {
                self.duration_s = (self.duration_s + 1).min(60);
            }
            KeyCode::Left if self.cross.join_focus == JoinFocus::Options => {
                self.duration_s = self.duration_s.saturating_sub(1).max(1);
            }
            KeyCode::Char(' ') if self.cross.join_focus == JoinFocus::Options => {
                self.cross.reverse = !self.cross.reverse;
            }
            KeyCode::Char(c)
                if self.cross.join_focus == JoinFocus::Target
                    && !c.is_whitespace()
                    && self.cross.code_input.len() < 255 =>
            {
                self.cross.code_input.push(c);
                self.cross.form_error.clear();
            }
            #[cfg(feature = "webrtc")]
            KeyCode::Backspace if self.cross.join_focus == JoinFocus::Signal => {
                self.cross.signal_url.pop();
                self.cross.form_error.clear();
            }
            #[cfg(feature = "webrtc")]
            KeyCode::Char(c)
                if self.cross.join_focus == JoinFocus::Signal
                    && !c.is_whitespace()
                    && self.cross.signal_url.len() < 1024 =>
            {
                self.cross.signal_url.push(c);
                self.cross.form_error.clear();
            }
            #[cfg(feature = "webrtc")]
            KeyCode::Backspace if self.cross.join_focus == JoinFocus::Stun => {
                self.cross.stun_urls.pop();
                self.cross.form_error.clear();
            }
            #[cfg(feature = "webrtc")]
            KeyCode::Char(c)
                if self.cross.join_focus == JoinFocus::Stun
                    && !c.is_whitespace()
                    && self.cross.stun_urls.len() < 2048 =>
            {
                self.cross.stun_urls.push(c);
                self.cross.form_error.clear();
            }
            KeyCode::Enter => {
                let input = self.cross.code_input.trim().to_string();
                if input.is_empty() {
                    self.cross.form_error = "target is required".into();
                    return;
                }
                let mode = self.cross.join_mode();
                if mode.needs_host()
                    && let Err(error) = parse_host_port(&input)
                {
                    self.cross.form_error = error;
                    return;
                }
                let transport_config = match self.cross.transport_config(mode) {
                    Ok(config) => config,
                    Err(error) => {
                        self.cross.form_error = error;
                        return;
                    }
                };
                self.cross.form_error.clear();
                self.reset_live("Join — connecting…");
                self.screen = Screen::Running;
                let (tx, rx) = unbounded_channel();
                self.rx = Some(rx);
                spawn_join(
                    mode,
                    input,
                    self.duration_s,
                    self.cross.reverse,
                    transport_config,
                    tx,
                );
            }
            _ => {}
        }
    }

    #[cfg(feature = "iroh")]
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
        #[cfg(feature = "webrtc")]
        let webrtc = matches!(self.cross.host_mode(), XportMode::WebRtc);
        #[cfg(not(feature = "webrtc"))]
        let webrtc = false;
        let [title, list_area, addr_area, stun_area, error_area, help] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
            Constraint::Length(if webrtc { 3 } else { 0 }),
            Constraint::Length(if self.cross.form_error.is_empty() {
                0
            } else {
                2
            }),
            Constraint::Length(1),
        ])
        .margin(1)
        .areas(area);
        #[cfg(not(feature = "webrtc"))]
        let _ = stun_area;

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
            .block(form_block(
                "transport",
                self.cross.host_focus == HostFocus::Transport,
            ))
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
            let cursor = if self.cross.host_focus == HostFocus::Address {
                "\u{2588}"
            } else {
                ""
            };
            #[cfg(feature = "quic")]
            let address_title = if matches!(self.cross.host_mode(), XportMode::Quic) {
                "host:port — self-signed benchmark certificate; peer unauthenticated"
            } else {
                "host:port  (editable)"
            };
            #[cfg(not(feature = "quic"))]
            let address_title = "host:port  (editable)";
            Paragraph::new(Line::from(vec![
                Span::styled("advertise  ", Style::new().fg(T.subtext)),
                Span::styled(
                    format!("{}{cursor}", self.cross.host_addr),
                    Style::new().fg(T.green),
                ),
            ]))
            .block(form_block(
                address_title,
                self.cross.host_focus == HostFocus::Address,
            ))
        } else if webrtc {
            #[cfg(feature = "webrtc")]
            {
                let cursor = if self.cross.host_focus == HostFocus::Signal {
                    "\u{2588}"
                } else {
                    ""
                };
                Paragraph::new(Span::styled(
                    format!("{}{cursor}", self.cross.signal_url),
                    Style::new().fg(T.green),
                ))
                .block(form_block(
                    "signal URL  (NETSU_SIGNAL_URL)",
                    self.cross.host_focus == HostFocus::Signal,
                ))
            }
            #[cfg(not(feature = "webrtc"))]
            unreachable!()
        } else {
            Paragraph::new(Span::styled(
                "iroh generates a self-describing ticket — nothing to enter",
                Style::new().fg(T.subtext).italic(),
            ))
            .block(rounded("address"))
        };
        f.render_widget(addr_para, addr_area);

        #[cfg(feature = "webrtc")]
        if webrtc {
            let cursor = if self.cross.host_focus == HostFocus::Stun {
                "\u{2588}"
            } else {
                ""
            };
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!("{}{cursor}", self.cross.stun_urls),
                    Style::new().fg(T.green),
                ))
                .block(form_block(
                    "STUN URLs  (comma-separated; empty disables)",
                    self.cross.host_focus == HostFocus::Stun,
                )),
                stun_area,
            );
        }
        if !self.cross.form_error.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled(
                    self.cross.form_error.clone(),
                    Style::new().fg(T.red),
                )),
                error_area,
            );
        }

        let help_items: &[(&str, &str)] = match self.cross.host_focus {
            HostFocus::Transport => &[
                ("tab", "next field"),
                ("↑/↓", "pick transport"),
                ("enter", "start"),
                ("esc", "back"),
            ],
            HostFocus::Address => &[
                ("tab", "next field"),
                ("type", "edit address"),
                ("backspace", "delete"),
                ("enter", "start"),
                ("esc", "back"),
            ],
            #[cfg(feature = "webrtc")]
            HostFocus::Signal | HostFocus::Stun => &[
                ("tab", "next field"),
                ("type", "edit field"),
                ("backspace", "delete"),
                ("enter", "start"),
                ("esc", "back"),
            ],
        };
        f.render_widget(help_bar(help_items), help);
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
        let socket = self.cross.host_mode().needs_host();
        let mut code_lines = Vec::new();
        if socket {
            // iperf3-style: the reachable address is what the peer types.
            if self.cross.host_addr_line.is_empty() {
                code_lines.push(Line::from(Span::styled(
                    format!("{spin} binding…"),
                    Style::new().fg(T.yellow),
                )));
            } else {
                code_lines.push(Line::from(vec![
                    Span::styled("listening  ", Style::new().fg(T.subtext)),
                    Span::styled(
                        self.cross.host_addr_line.clone(),
                        Style::new().fg(T.accent).bold(),
                    ),
                    Span::styled(
                        "   ← run a client against this",
                        Style::new().fg(T.subtext).italic(),
                    ),
                ]));
            }
        } else {
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
        }
        code_lines.push(Line::from(Span::styled(
            format!("{spin} {}", self.cross.host_status),
            Style::new().fg(T.green),
        )));
        #[cfg(feature = "quic")]
        if matches!(self.cross.host_mode(), XportMode::Quic) {
            code_lines.push(Line::from(Span::styled(
                "warning: certificate verification disabled; peer unauthenticated",
                Style::new().fg(T.yellow),
            )));
        }
        let host_title = if socket {
            "hosting — share this address"
        } else {
            "hosting — share the code"
        };
        f.render_widget(
            Paragraph::new(code_lines).block(rounded(host_title)),
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
        #[cfg(feature = "webrtc")]
        let webrtc = matches!(self.cross.join_mode(), XportMode::WebRtc);
        #[cfg(not(feature = "webrtc"))]
        let webrtc = false;
        let [
            title,
            list_area,
            input_area,
            signal_area,
            stun_area,
            opts_area,
            error_area,
            help,
        ] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
            Constraint::Length(if webrtc { 3 } else { 0 }),
            Constraint::Length(if webrtc { 3 } else { 0 }),
            Constraint::Length(3),
            Constraint::Length(if self.cross.form_error.is_empty() {
                0
            } else {
                2
            }),
            Constraint::Length(1),
        ])
        .margin(1)
        .areas(area);
        #[cfg(not(feature = "webrtc"))]
        let _ = (signal_area, stun_area);

        let socket = self.cross.join_mode().needs_host();
        f.render_widget(
            Paragraph::new(Span::styled(
                if socket {
                    "enter the host's address"
                } else {
                    "enter the host's code"
                },
                Style::new().fg(T.accent).bold(),
            ))
            .block(rounded("join a test")),
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
            .block(form_block(
                "transport  (match the host)",
                self.cross.join_focus == JoinFocus::Transport,
            ))
            .highlight_style(
                Style::new()
                    .fg(T.base)
                    .bg(T.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        let mut state = ListState::default();
        state.select(Some(self.cross.join_sel));
        f.render_stateful_widget(list, list_area, &mut state);

        let (field_label, input_title) = if socket {
            #[cfg(feature = "quic")]
            let title = if matches!(self.cross.join_mode(), XportMode::Quic) {
                "host:port — certificate verification disabled; peer unauthenticated"
            } else {
                "host:port  (e.g. 192.168.1.20:5201 or a public IP)"
            };
            #[cfg(not(feature = "quic"))]
            let title = "host:port  (e.g. 192.168.1.20:5201 or a public IP)";
            ("host  ", title)
        } else if webrtc {
            ("room  ", "room code  (shown by the WebRTC host)")
        } else {
            ("code  ", "code  (the short code the host is showing)")
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(field_label, Style::new().fg(T.subtext)),
                Span::styled(
                    format!(
                        "{}{}",
                        self.cross.code_input,
                        if self.cross.join_focus == JoinFocus::Target {
                            "\u{2588}"
                        } else {
                            ""
                        }
                    ),
                    Style::new().fg(T.green).bold(),
                ),
            ]))
            .block(form_block(
                input_title,
                self.cross.join_focus == JoinFocus::Target,
            )),
            input_area,
        );

        #[cfg(feature = "webrtc")]
        if webrtc {
            let signal_cursor = if self.cross.join_focus == JoinFocus::Signal {
                "\u{2588}"
            } else {
                ""
            };
            let stun_cursor = if self.cross.join_focus == JoinFocus::Stun {
                "\u{2588}"
            } else {
                ""
            };
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!("{}{signal_cursor}", self.cross.signal_url),
                    Style::new().fg(T.green),
                ))
                .block(form_block(
                    "signal URL  (NETSU_SIGNAL_URL)",
                    self.cross.join_focus == JoinFocus::Signal,
                )),
                signal_area,
            );
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!("{}{stun_cursor}", self.cross.stun_urls),
                    Style::new().fg(T.green),
                ))
                .block(form_block(
                    "STUN URLs  (comma-separated; empty disables)",
                    self.cross.join_focus == JoinFocus::Stun,
                )),
                stun_area,
            );
        }

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
            .block(form_block(
                "options  (←/→ duration, space reverse)",
                self.cross.join_focus == JoinFocus::Options,
            )),
            opts_area,
        );

        if !self.cross.form_error.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled(
                    self.cross.form_error.clone(),
                    Style::new().fg(T.red),
                )),
                error_area,
            );
        }

        let help_items: &[(&str, &str)] = match self.cross.join_focus {
            JoinFocus::Transport => &[
                ("tab", "next field"),
                ("↑/↓", "pick transport"),
                ("enter", "join"),
                ("esc", "back"),
            ],
            JoinFocus::Options => &[
                ("tab", "next field"),
                ("←/→", "duration"),
                ("space", "reverse"),
                ("enter", "join"),
                ("esc", "back"),
            ],
            JoinFocus::Target => &[
                ("tab", "next field"),
                ("type", if socket { "host" } else { "code" }),
                ("backspace", "delete"),
                ("enter", "join"),
                ("esc", "back"),
            ],
            #[cfg(feature = "webrtc")]
            JoinFocus::Signal | JoinFocus::Stun => &[
                ("tab", "next field"),
                ("type", "edit field"),
                ("backspace", "delete"),
                ("enter", "join"),
                ("esc", "back"),
            ],
        };
        f.render_widget(help_bar(help_items), help);
    }
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn err_summary(title: &str, detail: &str) -> Summary {
    Summary {
        title: title.into(),
        lines: vec![detail.into()],
        cli: String::new(),
        ok: false,
    }
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn client_error_summary(error: &netsu::error::NetsuError) -> Summary {
    if netsu::error::is_webrtc_direct_path_unavailable(error) {
        return Summary {
            title: "WebRTC direct connection unavailable".into(),
            lines: vec![netsu::error::WEBRTC_DIRECT_WARNING.into()],
            cli: String::new(),
            ok: false,
        };
    }
    err_summary("test failed", &error.to_string())
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn success_summary_lines(
    tag: &str,
    send_bits_per_second: f64,
    receive_bits_per_second: f64,
    sent_bytes: u64,
    received_bytes: u64,
) -> Vec<String> {
    let mut lines = vec![
        format!("transport {tag}"),
        format!("sent      {:.1} Mbit/s", send_bits_per_second / 1e6),
        format!("received  {:.1} Mbit/s", receive_bits_per_second / 1e6),
        format!("bytes     {sent_bytes} sent / {received_bytes} received"),
    ];
    if tag == "quic" {
        lines.push("warning: certificate verification disabled; peer unauthenticated".into());
    }
    lines
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn shell_arg(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '[' | ']')
        })
    {
        value.into()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn cli_hint(
    tag: &str,
    target: &str,
    port: u16,
    dur: u64,
    reverse: bool,
    config: &CrossTransportConfig,
) -> String {
    #[cfg(not(feature = "webrtc"))]
    let _ = config;
    let r = if reverse { " -R" } else { "" };
    let target = shell_arg(target);
    match tag {
        // iroh dials the ticket/code directly; sockets take host + -p port.
        "iroh" => format!("netsu client {target} --iroh -t {dur}{r}"),
        "quic" => format!("netsu client {target} -p {port} --quic --quic-insecure -t {dur}{r}"),
        "webrtc" => {
            #[cfg(feature = "webrtc")]
            {
                if let Some(options) = &config.webrtc {
                    let stun = options
                        .stun_urls
                        .iter()
                        .map(|url| format!(" --stun {}", shell_arg(url)))
                        .collect::<String>();
                    return format!(
                        "netsu client {target} --webrtc --signal-url {}{stun} -t {dur}{r}",
                        shell_arg(options.signal_url.as_str())
                    );
                }
            }
            format!("netsu client {target} --webrtc -t {dur}{r}")
        }
        "udp" => format!("netsu client {target} -p {port} -u -t {dur}{r}"),
        "ws" => format!("netsu client {target} -p {port} --ws -t {dur}{r}"),
        _ => format!("netsu client {target} -p {port} -t {dur}{r}"),
    }
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn default_advertise_host_with(lookup: impl FnOnce() -> Option<String>) -> String {
    lookup().unwrap_or_else(|| "127.0.0.1".into())
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn detect_local_ipv4() -> Option<String> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("8.8.8.8", 80)).ok()?;
    let std::net::IpAddr::V4(ip) = socket.local_addr().ok()?.ip() else {
        return None;
    };
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| ip.to_string())
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn parse_host_port(value: &str) -> Result<(String, u16), String> {
    let value = value.trim();
    let (host, port_text) = if let Some(rest) = value.strip_prefix('[') {
        let (host, suffix) = rest
            .split_once(']')
            .ok_or_else(|| "invalid host:port: missing ']' in IPv6 address".to_string())?;
        let port = suffix
            .strip_prefix(':')
            .ok_or_else(|| "invalid host:port: a port is required".to_string())?;
        (host, port)
    } else {
        let (host, port) = value
            .rsplit_once(':')
            .ok_or_else(|| "invalid host:port: expected host:port".to_string())?;
        if host.contains(':') {
            return Err("invalid host:port: wrap IPv6 addresses in [brackets]".into());
        }
        (host, port)
    };
    if host.is_empty() {
        return Err("invalid host:port: host is empty".into());
    }
    let port = port_text
        .parse::<u16>()
        .map_err(|_| "invalid host:port: port must be 1-65535".to_string())?;
    if port == 0 {
        return Err("invalid host:port: port must be 1-65535".into());
    }
    Ok((host.to_string(), port))
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
#[derive(Clone, Default)]
struct CrossTransportConfig {
    #[cfg(feature = "webrtc")]
    webrtc: Option<netsu::transport::webrtc::WebRtcOptions>,
}

#[cfg(feature = "webrtc")]
fn build_tui_webrtc_options(
    signal_url: &str,
    stun_urls: &str,
) -> Result<netsu::transport::webrtc::WebRtcOptions, String> {
    netsu::transport::webrtc::WebRtcOptions::new(
        signal_url.trim(),
        parse_stun_urls_field(stun_urls),
        false,
    )
    .map_err(|error| error.to_string())
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn server_options_for_mode(
    mode: XportMode,
    port: u16,
    config: &CrossTransportConfig,
    on_event: Option<netsu::server::ServerReporter>,
) -> Result<netsu::server::ServerOptions, String> {
    use netsu::client::Transport;
    #[cfg(not(feature = "webrtc"))]
    let _ = config;

    let transport = match mode {
        XportMode::Tcp | XportMode::Udp => Transport::Tcp,
        #[cfg(feature = "ws")]
        XportMode::Ws => Transport::Ws,
        #[cfg(feature = "iroh")]
        XportMode::Iroh => Transport::Iroh,
        #[cfg(feature = "quic")]
        XportMode::Quic => Transport::Quic,
        #[cfg(feature = "webrtc")]
        XportMode::WebRtc => Transport::WebRtc,
    };
    #[cfg(feature = "webrtc")]
    if matches!(mode, XportMode::WebRtc) && config.webrtc.is_none() {
        return Err("WebRTC signaling configuration is missing".into());
    }

    Ok(netsu::server::ServerOptions {
        port,
        transport,
        on_event,
        #[cfg(feature = "quic")]
        quic: matches!(mode, XportMode::Quic).then_some(netsu::server::QuicServerOptions {
            self_signed: true,
            cert_path: None,
            key_path: None,
        }),
        #[cfg(feature = "webrtc")]
        webrtc: if matches!(mode, XportMode::WebRtc) {
            config.webrtc.clone()
        } else {
            None
        },
        ..Default::default()
    })
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn client_options_for_mode(
    mode: XportMode,
    port: u16,
    duration_s: u64,
    reverse: bool,
    config: &CrossTransportConfig,
) -> Result<netsu::client::ClientOptions, String> {
    use netsu::client::Transport;
    #[cfg(not(feature = "webrtc"))]
    let _ = config;

    let (transport, udp) = match mode {
        XportMode::Tcp => (Transport::Tcp, false),
        XportMode::Udp => (Transport::Tcp, true),
        #[cfg(feature = "ws")]
        XportMode::Ws => (Transport::Ws, false),
        #[cfg(feature = "iroh")]
        XportMode::Iroh => (Transport::Iroh, false),
        #[cfg(feature = "quic")]
        XportMode::Quic => (Transport::Quic, false),
        #[cfg(feature = "webrtc")]
        XportMode::WebRtc => (Transport::WebRtc, false),
    };
    #[cfg(feature = "webrtc")]
    if matches!(mode, XportMode::WebRtc) && config.webrtc.is_none() {
        return Err("WebRTC signaling configuration is missing".into());
    }

    Ok(netsu::client::ClientOptions {
        port,
        transport,
        udp,
        reverse,
        duration: duration_s as u32,
        #[cfg(feature = "quic")]
        quic: matches!(mode, XportMode::Quic).then_some(netsu::client::QuicClientOptions {
            insecure: true,
            ca_path: None,
        }),
        #[cfg(feature = "webrtc")]
        webrtc: if matches!(mode, XportMode::WebRtc) {
            config.webrtc.clone()
        } else {
            None
        },
        ..Default::default()
    })
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn spawn_host(
    mode: XportMode,
    host_addr: String,
    transport_config: CrossTransportConfig,
    stop_rx: oneshot::Receiver<()>,
    tx: UnboundedSender<UiMsg>,
) {
    #[cfg(feature = "iroh")]
    use netsu::p2p::{addr, rendezkey};
    use netsu::server::{ServerEvent, ServerReporter, start_server};

    tokio::spawn(async move {
        let bind_port = if mode.needs_host() {
            match parse_host_port(&host_addr) {
                Ok((_, port)) => port,
                Err(error) => {
                    let _ = tx.send(UiMsg::HostFailed(error));
                    return;
                }
            }
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

        let options =
            match server_options_for_mode(mode, bind_port, &transport_config, Some(reporter)) {
                Ok(options) => options,
                Err(error) => {
                    let _ = tx.send(UiMsg::HostFailed(error));
                    return;
                }
            };
        let mut server = match start_server(options).await {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(UiMsg::HostFailed(format!("{e}")));
                return;
            }
        };

        // What a joiner needs to reach us: an iroh ticket, or host:bound-port.
        let addr_value = match mode {
            #[cfg(feature = "iroh")]
            XportMode::Iroh => server.endpoint_ticket.clone().unwrap_or_default(),
            #[cfg(feature = "webrtc")]
            XportMode::WebRtc => server.endpoint_ticket.clone().unwrap_or_default(),
            _ => {
                let host = parse_host_port(&host_addr)
                    .map(|(host, _)| host)
                    .unwrap_or_else(|_| host_addr.clone());
                if host.contains(':') {
                    format!("[{host}]:{}", server.port)
                } else {
                    format!("{host}:{}", server.port)
                }
            }
        };
        // Only iroh needs the rendez-key indirection: its ticket is a long,
        // opaque blob and the point is NAT traversal. Socket transports expose
        // a directly-dialable `host:port`, so — like iperf3 — we just show it
        // and the joiner types it straight in. No rendez-key round-trip.
        let (code, addr_line) = match mode {
            #[cfg(feature = "iroh")]
            XportMode::Iroh => {
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
                let line = match &code {
                    Some(_) => "iroh ticket — reachable across NAT/firewalls".to_string(),
                    None => format!("rendez-key unavailable — share manually: {addr_value}"),
                };
                (code, line)
            }
            #[cfg(feature = "webrtc")]
            XportMode::WebRtc => (
                Some(addr_value),
                "direct-only WebRTC — signaling carries no payload".to_string(),
            ),
            _ => (None, addr_value),
        };
        let _ = tx.send(UiMsg::HostReady { code, addr_line });

        // Socket and iroh servers remain live until stopped. A WebRTC room is
        // intentionally one-shot, so also surface its terminal session result.
        #[cfg(feature = "webrtc")]
        let one_shot = matches!(mode, XportMode::WebRtc);
        #[cfg(not(feature = "webrtc"))]
        let one_shot = false;
        let terminal = tokio::select! {
            _ = stop_rx => None,
            outcome = server.wait_terminal(), if one_shot => Some(outcome),
        };
        #[cfg(feature = "webrtc")]
        if let Some(outcome) = terminal {
            match outcome {
                Some(Ok(())) => {
                    let _ = tx.send(UiMsg::HostFinished);
                }
                Some(Err(error)) => {
                    let _ = tx.send(UiMsg::HostFailed(error.to_string()));
                }
                None => {
                    let _ = tx.send(UiMsg::HostFailed(
                        "WebRTC server ended without a terminal result".into(),
                    ));
                }
            }
        }
        #[cfg(not(feature = "webrtc"))]
        let _ = terminal;
        server.close().await;
    });
}

#[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
fn spawn_join(
    mode: XportMode,
    input: String,
    duration_s: u64,
    reverse: bool,
    transport_config: CrossTransportConfig,
    tx: UnboundedSender<UiMsg>,
) {
    use netsu::client::{Transport, run_client};
    #[cfg(feature = "iroh")]
    use netsu::p2p::{addr, rendezkey};

    // Map a rendezvous tag to a client transport + udp flag.
    #[cfg(feature = "iroh")]
    fn xport_of(tag: &str) -> Option<(Transport, bool)> {
        match tag {
            "tcp" => Some((Transport::Tcp, false)),
            "udp" => Some((Transport::Tcp, true)),
            #[cfg(feature = "ws")]
            "ws" => Some((Transport::Ws, false)),
            "iroh" => Some((Transport::Iroh, false)),
            _ => None,
        }
    }
    tokio::spawn(async move {
        // Resolve the joiner's input into a concrete dial target. iroh hides its
        // ticket behind a rendez-key code, so claim it first; socket transports
        // are dialed straight from the `host:port` the user typed — iperf3-style,
        // no rendez-key involved.
        let (tag, transport, udp, host, port, cli_target) = match mode {
            #[cfg(feature = "iroh")]
            XportMode::Iroh => {
                let blob = match rendezkey::claim(rendezkey::DEFAULT_BASE_URL, &input).await {
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
                let (transport, udp) = match xport_of(&tag) {
                    Some(x) => x,
                    None => {
                        let _ = tx.send(UiMsg::Done(err_summary(
                            "unsupported transport",
                            &format!(
                                "the code carried transport '{tag}', which this build can't dial"
                            ),
                        )));
                        return;
                    }
                };
                if tag == "iroh" {
                    (tag, transport, udp, addr_str, 0u16, input.clone())
                } else {
                    let (host, port) = match parse_host_port(&addr_str) {
                        Ok(target) => target,
                        Err(error) => {
                            let _ = tx.send(UiMsg::Done(err_summary("invalid target", &error)));
                            return;
                        }
                    };
                    let target = host.clone();
                    (tag, transport, udp, host, port, target)
                }
            }
            #[cfg(feature = "webrtc")]
            XportMode::WebRtc => (
                mode.tag().to_string(),
                Transport::WebRtc,
                false,
                input.clone(),
                0,
                input.clone(),
            ),
            socket_mode => {
                let (host, port) = match parse_host_port(&input) {
                    Ok(target) => target,
                    Err(error) => {
                        let _ = tx.send(UiMsg::Done(err_summary("invalid target", &error)));
                        return;
                    }
                };
                let (transport, udp) = match socket_mode {
                    XportMode::Udp => (Transport::Tcp, true),
                    #[cfg(feature = "ws")]
                    XportMode::Ws => (Transport::Ws, false),
                    #[cfg(feature = "quic")]
                    XportMode::Quic => (Transport::Quic, false),
                    XportMode::Tcp => (Transport::Tcp, false),
                    #[cfg(feature = "iroh")]
                    XportMode::Iroh => unreachable!(),
                    #[cfg(feature = "webrtc")]
                    XportMode::WebRtc => unreachable!(),
                };
                let target = host.clone();
                (
                    socket_mode.tag().to_string(),
                    transport,
                    udp,
                    host,
                    port,
                    target,
                )
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
        let mut opts =
            match client_options_for_mode(mode, port, duration_s, reverse, &transport_config) {
                Ok(options) => options,
                Err(error) => {
                    let _ = tx.send(UiMsg::Done(err_summary("invalid configuration", &error)));
                    return;
                }
            };
        opts.transport = transport;
        opts.udp = udp;
        let summary = match run_client(&host, opts, Some(on_interval)).await {
            Ok(r) => Summary {
                title: "test complete".into(),
                lines: success_summary_lines(
                    &tag,
                    r.send_bits_per_second,
                    r.receive_bits_per_second,
                    r.sent_bytes,
                    r.received_bytes,
                ),
                cli: cli_hint(
                    &tag,
                    &cli_target,
                    port,
                    duration_s,
                    reverse,
                    &transport_config,
                ),
                ok: true,
            },
            Err(e) => client_error_summary(&e),
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

    #[cfg(any(feature = "quic", feature = "webrtc"))]
    #[test]
    fn home_offers_cross_device_for_native_transport_builds() {
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
    fn joinconfig_iroh_prompts_for_a_code() {
        let mut app = App::new();
        app.screen = Screen::JoinConfig;
        // host_modes[0] is iroh, so join_sel 0 = the code flow.
        app.cross.join_sel = 0;
        app.cross.code_input = "7K3MQ9TX".into();
        let screen = rendered(&mut app);
        assert!(screen.contains("code"));
        assert!(screen.contains("7K3MQ9TX"));
    }

    #[cfg(feature = "iroh")]
    #[test]
    fn joinconfig_socket_prompts_for_a_host_address() {
        let mut app = App::new();
        app.screen = Screen::JoinConfig;
        // Select the first socket transport (TCP) — no rendez-key, iperf3-style.
        app.cross.join_sel = app
            .cross
            .host_modes
            .iter()
            .position(|m| m.needs_host())
            .expect("a socket transport exists");
        app.cross.code_input = "192.168.1.20:5201".into();
        let screen = rendered(&mut app);
        assert!(screen.contains("host"));
        assert!(screen.contains("192.168.1.20:5201"));
        // A LAN join must never ask for an 8-char rendez-key code.
        assert!(!screen.contains("short code the host"));
    }

    #[cfg(feature = "iroh")]
    #[test]
    fn hosting_socket_shows_address_not_a_code() {
        let mut app = App::new();
        app.screen = Screen::Hosting;
        // Pick a socket transport for hosting.
        app.cross.host_sel = app
            .cross
            .host_modes
            .iter()
            .position(|m| m.needs_host())
            .expect("a socket transport exists");
        app.cross.host_code = None;
        app.cross.host_addr_line = "192.168.1.20:5201".into();
        app.cross.host_status = "waiting for a peer to join…".into();
        let screen = rendered(&mut app);
        assert!(screen.contains("192.168.1.20:5201"));
        assert!(screen.contains("listening"));
        assert!(!screen.contains("publishing a code"));
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

    #[cfg(all(feature = "iroh", feature = "quic"))]
    #[test]
    fn hostconfig_lists_native_quic_separately_from_iroh() {
        let mut app = App::new();
        app.screen = Screen::HostConfig;
        let screen = rendered(&mut app);
        assert!(screen.contains("Native QUIC"));
        assert!(screen.contains("iroh / QUIC"));
    }

    #[cfg(all(feature = "iroh", feature = "webrtc"))]
    #[test]
    fn hostconfig_lists_direct_only_webrtc() {
        let mut app = App::new();
        app.screen = Screen::HostConfig;
        let screen = rendered(&mut app);
        assert!(screen.contains("WebRTC"));
        assert!(screen.contains("direct only"));
    }

    #[cfg(all(feature = "iroh", feature = "webrtc"))]
    #[test]
    fn webrtc_host_form_shows_public_signal_and_cloudflare_stun_defaults() {
        let mut app = App::new();
        app.screen = Screen::HostConfig;
        app.cross.host_sel = app
            .cross
            .host_modes
            .iter()
            .position(|mode| matches!(mode, XportMode::WebRtc))
            .unwrap();
        let screen = rendered(&mut app);
        assert!(screen.contains("https://rendez-key.xc.huakun.tech/v1/signal"));
        assert!(screen.contains("stun:stun.cloudflare.com:3478"));
        assert!(!screen.contains("TURN"));
    }

    #[cfg(all(feature = "iroh", feature = "webrtc"))]
    #[test]
    fn webrtc_join_form_shows_room_code_and_shared_signal_config() {
        let mut app = App::new();
        app.screen = Screen::JoinConfig;
        app.cross.join_sel = app
            .cross
            .host_modes
            .iter()
            .position(|mode| matches!(mode, XportMode::WebRtc))
            .unwrap();
        app.cross.code_input = "ABCD-EFGH".into();
        let screen = rendered(&mut app);
        assert!(screen.contains("ABCD-EFGH"));
        assert!(screen.contains("https://rendez-key.xc.huakun.tech/v1/signal"));
        assert!(screen.contains("stun:stun.cloudflare.com:3478"));
    }

    #[cfg(feature = "webrtc")]
    #[test]
    fn webrtc_defaults_can_be_overridden_without_exposing_secrets() {
        let (signal, stun) = webrtc_defaults_with(|name| match name {
            "NETSU_SIGNAL_URL" => Some("http://127.0.0.1:18787/v1/signal".into()),
            "NETSU_STUN_URLS" => Some(String::new()),
            _ => None,
        });
        assert_eq!(signal, "http://127.0.0.1:18787/v1/signal");
        assert!(stun.is_empty());
    }

    #[cfg(feature = "webrtc")]
    #[test]
    fn stun_field_splits_trims_and_drops_empty_values() {
        assert_eq!(
            parse_stun_urls_field(" stun:a.example:3478, ,stun:b.example:53 "),
            vec!["stun:a.example:3478", "stun:b.example:53"]
        );
    }

    #[cfg(feature = "webrtc")]
    #[test]
    fn web_rtc_host_fields_are_keyboard_editable() {
        let mut app = App::new();
        app.screen = Screen::HostConfig;
        app.cross.host_sel = app
            .cross
            .host_modes
            .iter()
            .position(|mode| matches!(mode, XportMode::WebRtc))
            .unwrap();

        app.on_hostconfig_key(KeyCode::Tab);
        assert_eq!(app.cross.host_focus, HostFocus::Signal);
        app.cross.signal_url.clear();
        for c in "https://signal.example/v1/signal".chars() {
            app.on_hostconfig_key(KeyCode::Char(c));
        }
        assert_eq!(app.cross.signal_url, "https://signal.example/v1/signal");

        app.on_hostconfig_key(KeyCode::Tab);
        assert_eq!(app.cross.host_focus, HostFocus::Stun);
        app.cross.stun_urls.clear();
        for c in "stun:stun.example:3478".chars() {
            app.on_hostconfig_key(KeyCode::Char(c));
        }
        assert_eq!(app.cross.stun_urls, "stun:stun.example:3478");
    }

    #[cfg(feature = "webrtc")]
    #[test]
    fn web_rtc_join_fields_and_options_are_keyboard_editable() {
        let mut app = App::new();
        app.screen = Screen::JoinConfig;
        app.cross.join_sel = app
            .cross
            .host_modes
            .iter()
            .position(|mode| matches!(mode, XportMode::WebRtc))
            .unwrap();

        app.on_joinconfig_key(KeyCode::Tab);
        assert_eq!(app.cross.join_focus, JoinFocus::Target);
        for c in "ROOM-123".chars() {
            app.on_joinconfig_key(KeyCode::Char(c));
        }
        assert_eq!(app.cross.code_input, "ROOM-123");

        app.on_joinconfig_key(KeyCode::Tab);
        app.on_joinconfig_key(KeyCode::Tab);
        app.on_joinconfig_key(KeyCode::Tab);
        assert_eq!(app.cross.join_focus, JoinFocus::Options);
        app.on_joinconfig_key(KeyCode::Char(' '));
        assert!(app.cross.reverse);
    }

    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
    #[test]
    fn socket_address_parser_rejects_missing_or_invalid_ports() {
        assert_eq!(
            parse_host_port("example.com:5201").unwrap(),
            ("example.com".into(), 5201)
        );
        assert_eq!(parse_host_port("[::1]:443").unwrap(), ("::1".into(), 443));
        assert!(parse_host_port("example.com").is_err());
        assert!(parse_host_port("example.com:nope").is_err());
        assert!(parse_host_port("example.com:0").is_err());
    }

    #[cfg(feature = "webrtc")]
    #[test]
    fn webrtc_cli_hint_reproduces_signal_and_every_stun_url_without_turn() {
        let config = CrossTransportConfig {
            webrtc: Some(
                build_tui_webrtc_options(
                    "https://signal.example/v1/signal",
                    "stun:a.example:3478,stun:b.example:53",
                )
                .unwrap(),
            ),
        };
        let hint = cli_hint("webrtc", "ROOM-123", 0, 5, false, &config);
        assert!(hint.contains("--signal-url https://signal.example/v1/signal"));
        assert!(hint.contains("--stun stun:a.example:3478"));
        assert!(hint.contains("--stun stun:b.example:53"));
        assert!(!hint.to_ascii_lowercase().contains("turn:"));
    }

    #[cfg(feature = "quic")]
    #[test]
    fn native_quic_screen_calls_out_benchmark_tls() {
        let mut app = App::new();
        app.screen = Screen::HostConfig;
        app.cross.host_sel = app
            .cross
            .host_modes
            .iter()
            .position(|mode| matches!(mode, XportMode::Quic))
            .unwrap();
        let screen = rendered(&mut app);
        assert!(screen.contains("self-signed benchmark certificate"));
    }

    #[cfg(feature = "quic")]
    #[test]
    fn native_quic_join_and_summary_disclose_unauthenticated_peer() {
        let mut app = App::new();
        app.screen = Screen::JoinConfig;
        app.cross.join_sel = app
            .cross
            .host_modes
            .iter()
            .position(|mode| matches!(mode, XportMode::Quic))
            .unwrap();
        assert!(rendered(&mut app).contains("peer unauthenticated"));

        let summary = success_summary_lines("quic", 1.0, 2.0, 3, 4);
        assert!(
            summary
                .iter()
                .any(|line| line.contains("verification disabled"))
        );
    }

    #[cfg(feature = "webrtc")]
    #[test]
    fn direct_path_failure_uses_the_stable_no_turn_warning() {
        let error = netsu::error::webrtc_setup_error(
            netsu::error::SetupPhase::IceConnected,
            netsu::error::WebRtcSetupFailure::DirectPathUnavailable,
        );
        let summary = client_error_summary(&error);
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line.contains("does not use TURN relay"))
        );
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line.contains("no throughput test was run"))
        );
    }

    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
    #[test]
    fn default_advertise_host_prefers_detected_lan_address() {
        assert_eq!(
            default_advertise_host_with(|| Some("192.168.50.7".into())),
            "192.168.50.7"
        );
        assert_eq!(default_advertise_host_with(|| None), "127.0.0.1");
    }

    #[cfg(any(feature = "iroh", feature = "quic", feature = "webrtc"))]
    #[test]
    fn empty_join_target_stays_on_form_with_an_error() {
        let mut app = App::new();
        app.screen = Screen::JoinConfig;
        app.cross.code_input.clear();
        app.on_joinconfig_key(KeyCode::Enter);
        assert!(matches!(app.screen, Screen::JoinConfig));
        assert_eq!(app.cross.form_error, "target is required");
    }

    #[cfg(feature = "webrtc")]
    #[test]
    fn cli_hint_shell_quotes_dynamic_values() {
        let config = CrossTransportConfig {
            webrtc: Some(
                build_tui_webrtc_options(
                    "https://signal.example/v1/signal?x=1&y=2",
                    "stun:stun.example:3478",
                )
                .unwrap(),
            ),
        };
        let hint = cli_hint("webrtc", "ROOM; touch /tmp/pwned", 0, 5, false, &config);
        assert!(hint.contains("'ROOM; touch /tmp/pwned'"));
        assert!(hint.contains("'https://signal.example/v1/signal?x=1&y=2'"));
    }

    #[cfg(feature = "webrtc")]
    #[test]
    fn completed_webrtc_host_marks_its_one_shot_room_consumed() {
        let mut app = App::new();
        app.screen = Screen::Hosting;
        app.cross.host_sel = app
            .cross
            .host_modes
            .iter()
            .position(|mode| matches!(mode, XportMode::WebRtc))
            .unwrap();
        app.on_msg(UiMsg::HostFinished);
        assert!(app.cross.host_status.contains("room consumed"));
        assert!(app.cross.host_stop.is_none());
    }

    #[cfg(feature = "webrtc")]
    #[test]
    fn webrtc_fields_build_validated_direct_only_options() {
        let options = build_tui_webrtc_options(
            "https://signal.example/v1/signal",
            "stun:a.example:3478, stun:b.example:53",
        )
        .unwrap();
        assert_eq!(
            options.signal_url.as_str(),
            "https://signal.example/v1/signal"
        );
        assert_eq!(
            options.stun_urls,
            ["stun:a.example:3478", "stun:b.example:53"]
        );
        assert!(!options.include_addresses);

        let error =
            build_tui_webrtc_options("https://signal.example/v1/signal", "turn:turn.example:3478")
                .unwrap_err();
        assert!(error.contains("TURN"));
    }

    #[cfg(feature = "quic")]
    #[test]
    fn native_quic_tui_uses_self_signed_server_and_explicit_insecure_client() {
        let config = CrossTransportConfig::default();
        let server = server_options_for_mode(XportMode::Quic, 5201, &config, None).unwrap();
        assert_eq!(server.transport, netsu::client::Transport::Quic);
        assert!(server.quic.unwrap().self_signed);

        let client = client_options_for_mode(XportMode::Quic, 5201, 5, false, &config).unwrap();
        assert_eq!(client.transport, netsu::client::Transport::Quic);
        assert!(client.quic.unwrap().insecure);
    }

    #[cfg(feature = "webrtc")]
    #[test]
    fn webrtc_tui_maps_shared_options_into_server_and_client() {
        let webrtc = build_tui_webrtc_options(
            "https://signal.example/v1/signal",
            "stun:stun.cloudflare.com:3478",
        )
        .unwrap();
        let config = CrossTransportConfig {
            webrtc: Some(webrtc.clone()),
        };
        let server = server_options_for_mode(XportMode::WebRtc, 0, &config, None).unwrap();
        assert_eq!(server.transport, netsu::client::Transport::WebRtc);
        assert_eq!(server.webrtc, Some(webrtc.clone()));

        let client = client_options_for_mode(XportMode::WebRtc, 0, 5, true, &config).unwrap();
        assert_eq!(client.transport, netsu::client::Transport::WebRtc);
        assert_eq!(client.webrtc, Some(webrtc));
        assert!(client.reverse);
    }

    #[cfg(feature = "webrtc")]
    #[tokio::test]
    async fn invalid_webrtc_host_config_stays_on_form_with_validation_error() {
        let mut app = App::new();
        app.screen = Screen::HostConfig;
        app.cross.host_sel = app
            .cross
            .host_modes
            .iter()
            .position(|mode| matches!(mode, XportMode::WebRtc))
            .unwrap();
        app.cross.signal_url = "not-a-url".into();
        app.start_hosting();
        assert!(matches!(app.screen, Screen::HostConfig));
        assert!(rendered(&mut app).contains("absolute HTTP(S) URL"));
    }

    #[cfg(feature = "webrtc")]
    #[tokio::test]
    async fn invalid_webrtc_join_config_stays_on_form_with_validation_error() {
        let mut app = App::new();
        app.screen = Screen::JoinConfig;
        app.cross.join_sel = app
            .cross
            .host_modes
            .iter()
            .position(|mode| matches!(mode, XportMode::WebRtc))
            .unwrap();
        app.cross.code_input = "ABCD-EFGH".into();
        app.cross.signal_url = "not-a-url".into();
        app.on_joinconfig_key(KeyCode::Enter);
        assert!(matches!(app.screen, Screen::JoinConfig));
        assert!(rendered(&mut app).contains("absolute HTTP(S) URL"));
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
