#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::logs::v1::LogRecord;
use prost::Message;

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

    pub fn take(&self) -> Vec<Request> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

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

pub fn log_resources(requests: &[Request]) -> Vec<Attributes> {
    requests
        .iter()
        .filter(|(path, _)| path == "/v1/logs")
        .flat_map(|(_, body)| {
            ExportLogsServiceRequest::decode(body.as_slice())
                .expect("a protobuf logs request")
                .resource_logs
        })
        .map(|r| strings(r.resource))
        .collect()
}

pub fn trace_resources(requests: &[Request]) -> Vec<(Attributes, Vec<String>)> {
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    requests
        .iter()
        .filter(|(path, _)| path == "/v1/traces")
        .flat_map(|(_, body)| {
            ExportTraceServiceRequest::decode(body.as_slice())
                .expect("a protobuf traces request")
                .resource_spans
        })
        .map(|r| {
            let names = r
                .scope_spans
                .into_iter()
                .flat_map(|s| s.spans)
                .map(|s| s.name)
                .collect();
            (strings(r.resource), names)
        })
        .collect()
}

pub type Attributes = std::collections::BTreeMap<String, String>;

fn strings(resource: Option<opentelemetry_proto::tonic::resource::v1::Resource>) -> Attributes {
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    resource
        .map(|r| r.attributes)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|kv| match kv.value.and_then(|v| v.value) {
            Some(Value::StringValue(s)) => Some((kv.key, s)),
            _ => None,
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct Span {
    pub service: String,
    pub name: String,
    pub trace_id: String,
    pub span_id: String,
    pub parent: String,
    pub attrs: std::collections::BTreeMap<String, String>,
}

pub fn spans(requests: &[Request]) -> Vec<Span> {
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use opentelemetry_proto::tonic::common::v1::{any_value::Value, KeyValue};
    let strings = |kvs: Vec<KeyValue>| -> std::collections::BTreeMap<String, String> {
        kvs.into_iter()
            .filter_map(|kv| match kv.value.and_then(|v| v.value) {
                Some(Value::StringValue(s)) => Some((kv.key, s)),
                _ => None,
            })
            .collect()
    };
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let mut out = Vec::new();
    for (_, body) in requests.iter().filter(|(path, _)| path == "/v1/traces") {
        let request =
            ExportTraceServiceRequest::decode(body.as_slice()).expect("a protobuf traces request");
        for resource_spans in request.resource_spans {
            let resource = strings(
                resource_spans
                    .resource
                    .map(|r| r.attributes)
                    .unwrap_or_default(),
            );
            let service = resource.get("service.name").cloned().unwrap_or_default();
            for span in resource_spans.scope_spans.into_iter().flat_map(|s| s.spans) {
                out.push(Span {
                    service: service.clone(),
                    name: span.name,
                    trace_id: hex(&span.trace_id),
                    span_id: hex(&span.span_id),
                    parent: hex(&span.parent_span_id),
                    attrs: strings(span.attributes),
                });
            }
        }
    }
    out
}
