//! `rvo-web` — a browser POC for RVO's pluggable model pipeline.
//!
//! Starts the orchestrator and a small HTTP server (reusing `tiny_http`) that
//! serves a single-page dashboard plus a JSON API. A new user can open the page
//! and:
//!   - watch the camera feed signals and metrics update live, and
//!   - **add a model node** (a gRPC endpoint + the signal it produces) and see
//!     RVO immediately start fanning frames out to it — no restart.
//!
//! Detectors/events are seeded from the YAML config (RVO_CONFIG or
//! config/rvo.yaml); nodes added in the browser are injected into the running
//! scheduler via `Scheduler::add_detector`.
//!
//! Open http://127.0.0.1:8080 (override with RVO_WEB_PORT). For a webcam-less
//! demo, point `camera.source_uri` at a video file in the config.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::bounded;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use rvo_bin::runtime::{build_camera_source, build_detectors, build_event_engine};
use rvo_buffer::FrameBuffer;
use rvo_camera::{start_camera, CameraConfig};
use rvo_clips::{start_encoder_worker, ClipManager};
use rvo_config::{try_load_config, RvoConfig};
use rvo_events::{Event, EventPublisher};
use rvo_metrics::{render_prometheus, METRICS};
use rvo_remote::RemoteGrpcDetector;
use rvo_scheduler::scheduler::Scheduler;
use rvo_signals::store::SignalType;

/// A model node as shown in the UI.
#[derive(Clone)]
struct NodeInfo {
    label: String,
    kind: String,
    endpoint: Option<String>,
    signal: Option<String>,
}

type Nodes = Arc<Mutex<Vec<NodeInfo>>>;
type Events = Arc<Mutex<VecDeque<Event>>>;

fn camera_label(cfg: &RvoConfig) -> String {
    match &cfg.camera.source_uri {
        Some(uri) => format!("uri: {uri}"),
        None => format!("device {}", cfg.camera.device_index.unwrap_or(0)),
    }
}

fn seed_nodes(cfg: &RvoConfig) -> Vec<NodeInfo> {
    cfg.detectors
        .iter()
        .filter(|d| d.enabled)
        .map(|d| {
            if d.kind == "remote_grpc" {
                let endpoint = d.endpoint.clone();
                let signal = d.output_signal.clone();
                NodeInfo {
                    label: format!(
                        "{} → {}",
                        endpoint.as_deref().unwrap_or("?"),
                        signal.as_deref().unwrap_or("?")
                    ),
                    kind: d.kind.clone(),
                    endpoint,
                    signal,
                }
            } else {
                NodeInfo {
                    label: d.kind.clone(),
                    kind: d.kind.clone(),
                    endpoint: None,
                    signal: None,
                }
            }
        })
        .collect()
}

fn build_state_json(
    scheduler: &Arc<Mutex<Scheduler>>,
    events: &Events,
    nodes: &Nodes,
    camera: &str,
    config_path: &str,
) -> Value {
    let snapshot = scheduler.lock().unwrap().signal_snapshot();
    let signals: Vec<Value> = snapshot
        .iter()
        .map(|(s, v)| json!({ "name": s.name(), "present": v.is_some(), "value": v }))
        .collect();

    let m = &*METRICS;
    let execs = m.detector_execs.load(Ordering::Relaxed);
    let avg_us = m
        .detector_exec_ns_total
        .load(Ordering::Relaxed)
        .checked_div(execs)
        .map(|ns| ns / 1000)
        .unwrap_or(0);
    let metrics = json!({
        "ticks": m.scheduler_ticks.load(Ordering::Relaxed),
        "detector_execs": execs,
        "detector_skips": m.detector_skips.load(Ordering::Relaxed),
        "detector_failures": m.detector_failures.load(Ordering::Relaxed),
        "events_emitted": m.events_emitted.load(Ordering::Relaxed),
        "frame_drops": m.frame_drops.load(Ordering::Relaxed),
        "avg_exec_us": avg_us,
    });

    let nodes_json: Vec<Value> = nodes
        .lock()
        .unwrap()
        .iter()
        .map(|n| json!({ "label": n.label, "kind": n.kind, "endpoint": n.endpoint, "signal": n.signal }))
        .collect();

    let events_json: Vec<Value> = events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .take(10)
        .map(|e| {
            json!({
                "type": format!("{:?}", e.event_type),
                "t": e.ts_ns as f64 / 1e9,
                "confidence": e.confidence,
            })
        })
        .collect();

    json!({
        "camera": camera,
        "config": config_path,
        "signals": signals,
        "metrics": metrics,
        "nodes": nodes_json,
        "events": events_json,
    })
}

fn respond(request: Request, status: u16, content_type: &str, body: String) {
    let header =
        Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes()).expect("valid header");
    let resp = Response::from_string(body)
        .with_status_code(status)
        .with_header(header);
    let _ = request.respond(resp);
}

/// Handle `POST /api/nodes` — add a remote gRPC model node to the running RVO.
fn handle_add_node(mut request: Request, scheduler: &Arc<Mutex<Scheduler>>, nodes: &Nodes) {
    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        respond(
            request,
            400,
            "application/json",
            json!({"ok": false, "error": "unreadable body"}).to_string(),
        );
        return;
    }

    let parsed: Result<Value, _> = serde_json::from_str(&body);
    let Ok(v) = parsed else {
        respond(
            request,
            400,
            "application/json",
            json!({"ok": false, "error": "invalid JSON"}).to_string(),
        );
        return;
    };

    let endpoint = v
        .get("endpoint")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let signal = v
        .get("signal")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    let Some(sig) = SignalType::from_name(&signal) else {
        respond(request, 400, "application/json", json!({"ok": false, "error": "signal must be one of Dummy|MotionLevel|FacePresent|PersonDetected"}).to_string());
        return;
    };
    if endpoint.is_empty() {
        respond(
            request,
            400,
            "application/json",
            json!({"ok": false, "error": "endpoint is required"}).to_string(),
        );
        return;
    }

    let idx = nodes.lock().unwrap().len();
    let id = format!("web-{signal}-{idx}");
    let detector = RemoteGrpcDetector::new(id, endpoint.clone(), sig, 15.0, 200, 1_000_000_000);
    scheduler.lock().unwrap().add_detector(Box::new(detector));
    nodes.lock().unwrap().push(NodeInfo {
        label: format!("{endpoint} → {signal}"),
        kind: "remote_grpc".to_string(),
        endpoint: Some(endpoint),
        signal: Some(signal),
    });

    respond(
        request,
        200,
        "application/json",
        json!({"ok": true}).to_string(),
    );
}

fn main() {
    // Keep remote-worker stderr quiet (a down node shouldn't spam the console).
    std::env::set_var("RVO_REMOTE_SILENT", "1");

    let config_path = std::env::var("RVO_CONFIG").unwrap_or_else(|_| "config/rvo.yaml".to_string());
    let port: u16 = std::env::var("RVO_WEB_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    let cfg = match try_load_config(&config_path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("[rvo-web] config error: {err}");
            std::process::exit(1);
        }
    };

    let camera = camera_label(&cfg);
    let nodes: Nodes = Arc::new(Mutex::new(seed_nodes(&cfg)));

    // ---- build the runtime (mirrors rvo-bin/main.rs) ----
    let detectors = match build_detectors(&cfg) {
        Ok(d) => d,
        Err(err) => {
            eprintln!("[rvo-web] {err}");
            std::process::exit(1);
        }
    };
    let event_engine = match build_event_engine(&cfg) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("[rvo-web] {err}");
            std::process::exit(1);
        }
    };

    let frame_buffer = Arc::new(Mutex::new(FrameBuffer::new(300)));

    let (frame_tx, frame_rx) = bounded(5);
    start_camera(
        CameraConfig {
            source: build_camera_source(&cfg),
        },
        frame_tx,
    );

    let (clip_tx, clip_rx) = bounded(8);
    start_encoder_worker(clip_rx, cfg.clips_dir.clone());
    let clip_manager = ClipManager::new(
        clip_tx,
        Duration::from_secs(3),
        Duration::from_secs(2),
        Arc::clone(&frame_buffer),
    );

    let (event_tx, event_rx) = bounded(64);
    let event_publisher = EventPublisher::new(event_tx);
    let events: Events = Arc::new(Mutex::new(VecDeque::new()));
    {
        let buf = Arc::clone(&events);
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

    {
        let sched = Arc::clone(&scheduler);
        thread::spawn(move || loop {
            sched.lock().unwrap().tick();
            thread::sleep(Duration::from_millis(1));
        });
    }

    // ---- HTTP server ----
    let server = match Server::http(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("[rvo-web] could not bind 127.0.0.1:{port}: {err}");
            std::process::exit(1);
        }
    };
    println!("[rvo-web] open http://127.0.0.1:{port}  (config={config_path})");

    for request in server.incoming_requests() {
        let method = request.method().clone();
        let url = request.url().to_string();
        match (method, url.as_str()) {
            (Method::Get, "/") => respond(
                request,
                200,
                "text/html; charset=utf-8",
                INDEX_HTML.to_string(),
            ),
            (Method::Get, "/api/state") => {
                let body = build_state_json(&scheduler, &events, &nodes, &camera, &config_path)
                    .to_string();
                respond(request, 200, "application/json", body);
            }
            (Method::Get, "/metrics") => respond(request, 200, "text/plain", render_prometheus()),
            (Method::Post, "/api/nodes") => handle_add_node(request, &scheduler, &nodes),
            _ => respond(request, 404, "text/plain", "not found".to_string()),
        }
    }
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>RVO — live</title>
<style>
  :root { color-scheme: dark; }
  body { font-family: ui-monospace, Menlo, Consolas, monospace; margin: 0; background:#0e1116; color:#d7dde3; }
  header { padding:12px 18px; border-bottom:1px solid #232a33; }
  header h1 { margin:0; font-size:16px; }
  header .sub { color:#8b97a5; font-size:12px; margin-top:4px; }
  main { display:grid; grid-template-columns: 1fr 1fr; gap:14px; padding:14px 18px; }
  .card { background:#151a21; border:1px solid #232a33; border-radius:8px; padding:12px 14px; }
  .card h2 { margin:0 0 10px; font-size:13px; color:#9fb0c0; text-transform:uppercase; letter-spacing:.5px; }
  .chip { display:inline-flex; align-items:center; gap:6px; padding:5px 9px; margin:3px; border-radius:6px; background:#1b222b; border:1px solid #2a323d; }
  .dot { width:9px; height:9px; border-radius:50%; background:#3a4250; }
  .dot.on { background:#39d353; box-shadow:0 0 6px #39d353; }
  table { width:100%; border-collapse:collapse; font-size:13px; }
  td { padding:3px 0; }
  td.k { color:#8b97a5; } td.v { text-align:right; }
  ul { list-style:none; margin:0; padding:0; font-size:13px; }
  li { padding:4px 0; border-bottom:1px dashed #232a33; }
  .node { display:flex; align-items:center; gap:8px; padding:5px 0; border-bottom:1px dashed #232a33; }
  form { margin-top:10px; display:flex; gap:8px; flex-wrap:wrap; }
  input, select, button { font:inherit; background:#0e1116; color:#d7dde3; border:1px solid #2a323d; border-radius:6px; padding:6px 8px; }
  button { background:#1f6feb; border-color:#1f6feb; color:#fff; cursor:pointer; }
  button:hover { background:#2a7df5; }
  #msg { font-size:12px; margin-top:6px; min-height:14px; }
  .err { color:#ff7b72; } .ok { color:#39d353; }
  .full { grid-column:1 / -1; }
</style>
</head>
<body>
<header>
  <h1>RVO — RealTime Vision Orchestrator</h1>
  <div class="sub" id="hdr">connecting…</div>
</header>
<main>
  <section class="card full">
    <h2>Model nodes</h2>
    <div id="nodes"></div>
    <form id="addForm">
      <input id="endpoint" placeholder="http://localhost:50051" size="28" required>
      <select id="signal">
        <option>PersonDetected</option>
        <option>FacePresent</option>
        <option>MotionLevel</option>
        <option>Dummy</option>
      </select>
      <button type="submit">Add node</button>
    </form>
    <div id="msg"></div>
  </section>

  <section class="card">
    <h2>Signals</h2>
    <div id="signals"></div>
  </section>

  <section class="card">
    <h2>Metrics</h2>
    <table id="metrics"></table>
  </section>

  <section class="card full">
    <h2>Recent events</h2>
    <ul id="events"></ul>
  </section>
</main>

<script>
function el(tag, cls, txt){ const e=document.createElement(tag); if(cls)e.className=cls; if(txt!=null)e.textContent=txt; return e; }

async function refresh(){
  let s;
  try { s = await (await fetch('/api/state')).json(); }
  catch(e){ document.getElementById('hdr').textContent = 'disconnected'; return; }

  document.getElementById('hdr').textContent = 'camera: ' + s.camera + '   •   config: ' + s.config;

  const sig = document.getElementById('signals'); sig.innerHTML='';
  for(const x of s.signals){
    const chip = el('span','chip');
    chip.appendChild(el('span','dot'+(x.present?' on':'')));
    chip.appendChild(el('span',null, x.name + (x.present? ' = '+x.value : ' —')));
    sig.appendChild(chip);
  }

  const present = {}; s.signals.forEach(x=>present[x.name]=x.present);
  const nodes = document.getElementById('nodes'); nodes.innerHTML='';
  if(s.nodes.length===0) nodes.appendChild(el('div',null,'(no nodes)'));
  for(const n of s.nodes){
    const row = el('div','node');
    const lit = n.signal && present[n.signal];
    row.appendChild(el('span','dot'+(lit?' on':'')));
    row.appendChild(el('span',null, n.label + '  ['+n.kind+']'));
    nodes.appendChild(row);
  }

  const m = s.metrics;
  const rows = [['ticks',m.ticks],['detector_execs',m.detector_execs],['detector_skips',m.detector_skips],
    ['detector_failures',m.detector_failures],['avg exec µs',m.avg_exec_us],['events_emitted',m.events_emitted],['frame_drops',m.frame_drops]];
  const mt = document.getElementById('metrics'); mt.innerHTML='';
  for(const [k,v] of rows){ const tr=el('tr'); tr.appendChild(el('td','k',k)); tr.appendChild(el('td','v',String(v))); mt.appendChild(tr); }

  const ev = document.getElementById('events'); ev.innerHTML='';
  if(s.events.length===0) ev.appendChild(el('li',null,'(no events yet)'));
  for(const e of s.events){ ev.appendChild(el('li',null, e.type+'   t='+e.t.toFixed(3)+'s   conf='+e.confidence.toFixed(2))); }
}

document.getElementById('addForm').addEventListener('submit', async (e)=>{
  e.preventDefault();
  const endpoint = document.getElementById('endpoint').value.trim();
  const signal = document.getElementById('signal').value;
  const msg = document.getElementById('msg'); msg.textContent='adding…'; msg.className='';
  try {
    const r = await fetch('/api/nodes', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({endpoint, signal})});
    const j = await r.json();
    if(j.ok){ msg.textContent='added '+endpoint+' → '+signal; msg.className='ok'; document.getElementById('endpoint').value=''; }
    else { msg.textContent='error: '+j.error; msg.className='err'; }
  } catch(err){ msg.textContent='request failed'; msg.className='err'; }
  refresh();
});

refresh();
setInterval(refresh, 700);
</script>
</body>
</html>
"#;
