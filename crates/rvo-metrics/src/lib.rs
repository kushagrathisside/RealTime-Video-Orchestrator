use std::thread;
use tiny_http::{Server, Response};

mod metrics;
pub use metrics::{METRICS, render_prometheus};

pub fn start_metrics_server(port: u16) {
    thread::spawn(move || {
        let server =
            Server::http(format!("127.0.0.1:{}", port))
                .expect("metrics server");

        for req in server.incoming_requests() {
            if req.url() == "/metrics" {
                let body = render_prometheus();
                let _ = req.respond(Response::from_string(body));
            } else {
                let _ = req.respond(Response::from_string("404"));
            }
        }
    });
}
