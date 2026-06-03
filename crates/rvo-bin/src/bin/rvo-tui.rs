//! `rvo-tui` — an interactive terminal app for RVO.
//!
//! Two phases:
//!   1. Menu — pick a camera source (probed local devices or the config default).
//!   2. Dashboard — live view of services, signals, metrics, and recent events
//!      while the orchestrator runs.
//!
//! Detectors and events come from the YAML config (RVO_CONFIG or
//! config/rvo.yaml); the menu only chooses the camera. Quit with `q` / Esc.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::bounded;

use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};

/// Terminal that renders to the controlling tty. The writer is boxed so the
/// unix (`/dev/tty`) and fallback (stdout) paths share one concrete type.
type Tui = Terminal<CrosstermBackend<Box<dyn Write + Send>>>;

use rvo_bin::runtime::{build_camera_source, build_detectors, build_event_engine};
use rvo_buffer::FrameBuffer;
use rvo_camera::{list_cameras, start_camera, CameraConfig, CameraSource};
use rvo_clips::{start_encoder_worker, ClipManager};
use rvo_config::{try_load_config, RvoConfig};
use rvo_events::{Event, EventPublisher};
use rvo_metrics::METRICS;
use rvo_scheduler::scheduler::Scheduler;

/// A selectable camera source in the menu.
enum CameraChoice {
    Device(i32),
    ConfigDefault,
}

impl CameraChoice {
    fn label(&self, cfg: &RvoConfig) -> String {
        match self {
            CameraChoice::Device(i) => format!("Local camera device {i}"),
            CameraChoice::ConfigDefault => match &cfg.camera.source_uri {
                Some(uri) => format!("Config default (uri: {uri})"),
                None => format!(
                    "Config default (device {})",
                    cfg.camera.device_index.unwrap_or(0)
                ),
            },
        }
    }

    fn to_source(&self, cfg: &RvoConfig) -> CameraSource {
        match self {
            CameraChoice::Device(i) => CameraSource::Device(*i),
            CameraChoice::ConfigDefault => build_camera_source(cfg),
        }
    }
}

/// One-line description per configured detector, for the dashboard.
fn describe_detectors(cfg: &RvoConfig) -> Vec<String> {
    cfg.detectors
        .iter()
        .filter(|d| d.enabled)
        .map(|d| match d.kind.as_str() {
            "remote_grpc" => format!(
                "{} → {} [{}]",
                d.endpoint.as_deref().unwrap_or("?"),
                d.output_signal.as_deref().unwrap_or("?"),
                d.kind
            ),
            other => other.to_string(),
        })
        .collect()
}

/// Enter the TUI: pick a render target on the real terminal and redirect this
/// process's stdout(1)+stderr(2) to `rvo-tui.log`. The fd-level redirect is the
/// only thing that also captures OpenCV's C++ warnings (which bypass Rust's
/// `println!`/`eprintln!`) and the camera/clip log lines, so they can't corrupt
/// the screen.
fn setup_terminal() -> std::io::Result<Tui> {
    let writer: Box<dyn Write + Send> = {
        #[cfg(unix)]
        {
            use std::fs::OpenOptions;
            use std::os::unix::io::AsRawFd;

            if let Ok(log) = OpenOptions::new()
                .create(true)
                .append(true)
                .open("rvo-tui.log")
            {
                unsafe {
                    libc::dup2(log.as_raw_fd(), libc::STDOUT_FILENO);
                    libc::dup2(log.as_raw_fd(), libc::STDERR_FILENO);
                }
                // fds 1/2 now alias the log; the original handle can close.
            }

            match OpenOptions::new().read(true).write(true).open("/dev/tty") {
                Ok(tty) => Box::new(tty),
                Err(_) => Box::new(std::io::stdout()),
            }
        }
        #[cfg(not(unix))]
        {
            Box::new(std::io::stdout())
        }
    };

    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(writer))?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Tui) {
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

fn main() {
    // Silence the remote workers' Rust-side logging too (belt and suspenders
    // alongside the fd redirect below).
    std::env::set_var("RVO_REMOTE_SILENT", "1");

    let config_path = std::env::var("RVO_CONFIG").unwrap_or_else(|_| "config/rvo.yaml".to_string());
    let cfg = match try_load_config(&config_path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("[rvo-tui] config error: {err}");
            std::process::exit(1);
        }
    };

    let service_lines = describe_detectors(&cfg);

    // Probe cameras BEFORE entering the alternate screen (opening devices is slow).
    println!("[rvo-tui] probing cameras …");
    let mut choices: Vec<CameraChoice> = list_cameras(10)
        .into_iter()
        .map(CameraChoice::Device)
        .collect();
    choices.push(CameraChoice::ConfigDefault);

    // Save the real stdout/stderr so we can restore them after the TUI exits and
    // any final error is visible to the user.
    #[cfg(unix)]
    let saved_fds = unsafe {
        (
            libc::dup(libc::STDOUT_FILENO),
            libc::dup(libc::STDERR_FILENO),
        )
    };

    let mut terminal = match setup_terminal() {
        Ok(t) => t,
        Err(err) => {
            eprintln!("[rvo-tui] terminal init failed: {err}");
            std::process::exit(1);
        }
    };
    let outcome = run_app(&mut terminal, &cfg, &choices, &service_lines, &config_path);
    restore_terminal(&mut terminal);

    // Restore the original stdout/stderr (the TUI redirected them to the log).
    #[cfg(unix)]
    unsafe {
        libc::dup2(saved_fds.0, libc::STDOUT_FILENO);
        libc::dup2(saved_fds.1, libc::STDERR_FILENO);
        libc::close(saved_fds.0);
        libc::close(saved_fds.1);
    }

    if let Err(err) = outcome {
        eprintln!("[rvo-tui] {err}");
        std::process::exit(1);
    }
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    cfg: &RvoConfig,
    choices: &[CameraChoice],
    service_lines: &[String],
    config_path: &str,
) -> Result<(), String> {
    let selected = run_menu(terminal, cfg, choices, service_lines, config_path)
        .map_err(|e| format!("menu io: {e}"))?;

    let Some(idx) = selected else {
        return Ok(()); // user quit from the menu
    };

    let source = choices[idx].to_source(cfg);
    let camera_label = choices[idx].label(cfg);

    // ---- build the runtime (mirrors rvo-bin/main.rs) ----
    let detectors = build_detectors(cfg)?;
    let event_engine = build_event_engine(cfg)?;

    let frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(300)));

    let (frame_tx, frame_rx) = bounded(5);
    start_camera(CameraConfig { source }, frame_tx);

    let (clip_tx, clip_rx) = bounded(8);
    start_encoder_worker(clip_rx, cfg.clips_dir.clone());
    let clip_manager = ClipManager::new(
        clip_tx,
        Duration::from_secs(3),
        Duration::from_secs(2),
        Arc::clone(&frame_buffer),
    );

    // Tap the event stream into a ring buffer the dashboard reads.
    let (event_tx, event_rx) = bounded(64);
    let event_publisher = EventPublisher::new(event_tx);
    let events_buf: Arc<Mutex<VecDeque<Event>>> = Arc::new(Mutex::new(VecDeque::new()));
    {
        let buf = Arc::clone(&events_buf);
        thread::spawn(move || {
            while let Ok(ev) = event_rx.recv() {
                let mut b = buf.lock().unwrap();
                if b.len() >= 50 {
                    b.pop_front();
                }
                b.push_back(ev);
            }
        });
    }

    let scheduler = Arc::new(Mutex::new(Scheduler::new(
        detectors,
        event_engine,
        frame_rx,
        clip_manager,
        event_publisher,
        frame_buffer,
    )));

    // Run the tick loop off the UI thread.
    {
        let sched = Arc::clone(&scheduler);
        thread::spawn(move || loop {
            sched.lock().unwrap().tick();
            thread::sleep(Duration::from_millis(1));
        });
    }

    run_dashboard(
        terminal,
        &scheduler,
        &events_buf,
        service_lines,
        &camera_label,
        config_path,
    )
    .map_err(|e| format!("dashboard io: {e}"))
}

/// Menu: choose a camera source. Returns `Some(index)` to start, `None` to quit.
fn run_menu<B: Backend>(
    terminal: &mut Terminal<B>,
    cfg: &RvoConfig,
    choices: &[CameraChoice],
    service_lines: &[String],
    config_path: &str,
) -> std::io::Result<Option<usize>> {
    let mut selected = 0usize;

    loop {
        terminal.draw(|f| draw_menu(f, cfg, choices, service_lines, config_path, selected))?;

        if event::poll(Duration::from_millis(150))? {
            if let TermEvent::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') if selected + 1 < choices.len() => {
                        selected += 1;
                    }
                    KeyCode::Enter => return Ok(Some(selected)),
                    _ => {}
                }
            }
        }
    }
}

fn draw_menu(
    f: &mut Frame,
    cfg: &RvoConfig,
    choices: &[CameraChoice],
    service_lines: &[String],
    config_path: &str,
    selected: usize,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(f.area());

    let header = Paragraph::new(format!("RVO — config: {config_path}")).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" RealTime Vision Orchestrator "),
    );
    f.render_widget(header, rows[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(rows[1]);

    let items: Vec<ListItem> = choices
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let marker = if i == selected { "▶ " } else { "  " };
            let style = if i == selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(format!("{marker}{}", c.label(cfg))).style(style)
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Select a camera "),
    );
    f.render_widget(list, cols[0]);

    let services: Vec<Line> = if service_lines.is_empty() {
        vec![Line::from("(none configured)")]
    } else {
        service_lines
            .iter()
            .map(|s| Line::from(s.clone()))
            .collect()
    };
    let svc = Paragraph::new(services).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Services / detectors "),
    );
    f.render_widget(svc, cols[1]);

    let footer = Paragraph::new("↑/↓ or j/k to move • Enter to start • q to quit")
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, rows[2]);
}

/// Live dashboard. Returns when the user quits.
fn run_dashboard<B: Backend>(
    terminal: &mut Terminal<B>,
    scheduler: &Arc<Mutex<Scheduler>>,
    events_buf: &Arc<Mutex<VecDeque<Event>>>,
    service_lines: &[String],
    camera_label: &str,
    config_path: &str,
) -> std::io::Result<()> {
    loop {
        terminal.draw(|f| {
            draw_dashboard(
                f,
                scheduler,
                events_buf,
                service_lines,
                camera_label,
                config_path,
            )
        })?;

        if event::poll(Duration::from_millis(150))? {
            if let TermEvent::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press
                    && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                {
                    return Ok(());
                }
            }
        }
    }
}

fn draw_dashboard(
    f: &mut Frame,
    scheduler: &Arc<Mutex<Scheduler>>,
    events_buf: &Arc<Mutex<VecDeque<Event>>>,
    service_lines: &[String],
    camera_label: &str,
    config_path: &str,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(10),
            Constraint::Length(3),
        ])
        .split(f.area());

    // Header
    let header = Paragraph::new(format!("camera: {camera_label}    config: {config_path}"))
        .block(Block::default().borders(Borders::ALL).title(" RVO — live "));
    f.render_widget(header, rows[0]);

    // Middle: services | signals | metrics
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(38),
            Constraint::Percentage(30),
            Constraint::Percentage(32),
        ])
        .split(rows[1]);

    let svc: Vec<Line> = if service_lines.is_empty() {
        vec![Line::from("(none)")]
    } else {
        service_lines
            .iter()
            .map(|s| Line::from(s.clone()))
            .collect()
    };
    f.render_widget(
        Paragraph::new(svc).block(Block::default().borders(Borders::ALL).title(" Services ")),
        cols[0],
    );

    // Signals snapshot (locks the scheduler briefly).
    let snapshot = scheduler.lock().unwrap().signal_snapshot();
    let signal_lines: Vec<Line> = snapshot
        .iter()
        .map(|(sig, val)| match val {
            Some(v) => Line::from(vec![
                Span::styled("● ", Style::default().fg(Color::Green)),
                Span::raw(format!("{:<14} = {v}", sig.name())),
            ]),
            None => Line::from(vec![
                Span::styled("○ ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:<14} —", sig.name()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
        })
        .collect();
    f.render_widget(
        Paragraph::new(signal_lines)
            .block(Block::default().borders(Borders::ALL).title(" Signals ")),
        cols[1],
    );

    // Metrics (global atomics).
    let m = &*METRICS;
    let execs = m.detector_execs.load(Ordering::Relaxed);
    let exec_ns = m.detector_exec_ns_total.load(Ordering::Relaxed);
    let avg_us = exec_ns.checked_div(execs).map(|ns| ns / 1000).unwrap_or(0);
    let metric_lines = vec![
        Line::from(format!(
            "ticks         {}",
            m.scheduler_ticks.load(Ordering::Relaxed)
        )),
        Line::from(format!("detector_exec {execs}")),
        Line::from(format!(
            "detector_skip {}",
            m.detector_skips.load(Ordering::Relaxed)
        )),
        Line::from(vec![Span::styled(
            format!(
                "detector_fail {}",
                m.detector_failures.load(Ordering::Relaxed)
            ),
            Style::default().fg(Color::Red),
        )]),
        Line::from(format!("avg exec      {avg_us} µs")),
        Line::from(vec![Span::styled(
            format!("events        {}", m.events_emitted.load(Ordering::Relaxed)),
            Style::default().fg(Color::Yellow),
        )]),
        Line::from(format!(
            "frame_drops   {}",
            m.frame_drops.load(Ordering::Relaxed)
        )),
        Line::from(format!(
            "clip_drops    {}",
            m.clip_drops.load(Ordering::Relaxed)
        )),
    ];
    f.render_widget(
        Paragraph::new(metric_lines)
            .block(Block::default().borders(Borders::ALL).title(" Metrics ")),
        cols[2],
    );

    // Recent events.
    let events = events_buf.lock().unwrap();
    let event_lines: Vec<Line> = if events.is_empty() {
        vec![Line::from("(no events yet)")]
    } else {
        events
            .iter()
            .rev()
            .take(8)
            .map(|e| {
                Line::from(format!(
                    "{:?}  t={:.3}s  conf={:.2}",
                    e.event_type,
                    e.ts_ns as f64 / 1e9,
                    e.confidence
                ))
            })
            .collect()
    };
    f.render_widget(
        Paragraph::new(event_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Recent events "),
        ),
        rows[2],
    );

    let footer = Paragraph::new("q / Esc to quit").block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, rows[3]);
}
