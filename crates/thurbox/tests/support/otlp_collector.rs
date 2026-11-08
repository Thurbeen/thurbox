//! A minimal in-process OTLP/HTTP collector, for the tests that assert what
//! thurbox exports: it records every POST and answers with whatever status the
//! test last set, so a test can make it refuse an export.
//!
//! Each test file that includes it uses a different part of it.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::logs::v1::LogRecord;
use prost::Message;

/// One request the receiver took: the path it was posted to and its body.
pub type Request = (String, Vec<u8>);

pub struct Collector {
    port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
    status: Arc<AtomicU16>,
}

impl Collector {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the receiver");
        let port = listener.local_addr().expect("receiver address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let status = Arc::new(AtomicU16::new(200));
        let (seen, answer) = (requests.clone(), status.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (seen, answer) = (seen.clone(), answer.clone());
                std::thread::spawn(move || serve(stream, &seen, &answer));
            }
        });
        Self {
            port,
            requests,
            status,
        }
    }

    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn answer_with(&self, status: u16) {
        self.status.store(status, Ordering::SeqCst);
    }

    /// Everything received since the last call.
    pub fn take(&self) -> Vec<Request> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

/// Answer HTTP/1.1 requests on one connection until the client closes it.
fn serve(stream: TcpStream, seen: &Mutex<Vec<Request>>, answer: &AtomicU16) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone the stream"));
    let mut writer = stream;
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
            return;
        }
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let mut length = 0usize;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                return;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        seen.lock().unwrap().push((path, body));
        let status = answer.load(Ordering::SeqCst);
        let response = format!("HTTP/1.1 {status} Status\r\ncontent-length: 0\r\n\r\n");
        if writer.write_all(response.as_bytes()).is_err() {
            return;
        }
    }
}

/// Every log record in the `/v1/logs` requests among `requests`.
pub fn logs(requests: &[Request]) -> Vec<LogRecord> {
    requests
        .iter()
        .filter(|(path, _)| path == "/v1/logs")
        .flat_map(|(_, body)| {
            ExportLogsServiceRequest::decode(body.as_slice())
                .expect("a protobuf logs request")
                .resource_logs
        })
        .flat_map(|r| r.scope_logs)
        .flat_map(|s| s.log_records)
        .collect()
}
