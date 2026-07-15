use std::thread;
use tiny_http::{Response, Server};

mod metrics;
pub use metrics::{render_prometheus, METRICS};

pub fn start_metrics_server(port: u16) {
    thread::spawn(move || {
        let server = match Server::http(format!("127.0.0.1:{}", port)) {
            Ok(s) => s,
            Err(err) => {
                eprintln!(
                    "[METRICS] Could not bind 127.0.0.1:{port}: {err} — metrics/health endpoint disabled"
                );
                return;
            }
        };

        eprintln!("[METRICS] Listening on http://127.0.0.1:{port}/metrics");

        for req in server.incoming_requests() {
            match req.url() {
                "/metrics" => {
                    let body = render_prometheus();
                    let _ = req.respond(Response::from_string(body));
                }
                "/health" => {
                    // Lightweight liveness probe — always returns 200 while
                    // the process is alive. A richer readiness check (camera
                    // alive, scheduler running) belongs in a future /ready
                    // endpoint.
                    let _ = req.respond(Response::from_string("ok"));
                }
                _ => {
                    let _ = req.respond(Response::from_string("not found").with_status_code(404));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn port_conflict_does_not_panic() {
        // Bind a server on a fixed port first, then call start_metrics_server on
        // the same port. The function must return without panicking and the calling
        // thread must remain alive — previously this caused a process abort via
        // .expect("metrics server").
        let port: u16 = 19199;
        let _holder = Server::http(format!("127.0.0.1:{port}")).expect("test holder");

        // This must not panic even though the port is occupied.
        start_metrics_server(port);

        // Give the background thread time to attempt the bind and exit cleanly.
        thread::sleep(Duration::from_millis(50));

        // If we reach here the main thread is still alive — test passes.
    }
}
