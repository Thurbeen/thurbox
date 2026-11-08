//! An in-process OTLP/HTTP collector for the telemetry acceptance: a plain
//! TCP listener that answers every request `200 OK`, keeps the protobuf
//! bodies by path, and decodes them into flat spans and data points that a
//! test can search. It speaks only what Fleet's exporter sends.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::metrics::v1::metric;
use prost::Message;

type Attrs = BTreeMap<String, String>;
/// Each request's path and body, in arrival order.
type Received = Mutex<Vec<(String, Vec<u8>)>>;

pub struct Collector {
    pub endpoint: String,
    received: Arc<Received>,
}

/// One exported span, its attributes and its resource's, as strings.
#[derive(Debug, Clone)]
pub struct Span {
    pub name: String,
    pub trace_id: Vec<u8>,
    pub span_id: Vec<u8>,
    pub parent: Vec<u8>,
    pub error: bool,
    pub attrs: Attrs,
    pub resource: Attrs,
}

/// One exported data point of a metric, with its point attributes.
#[derive(Debug, Clone)]
pub struct Point {
    pub metric: String,
    pub unit: String,
    pub attrs: Attrs,
    /// The sum, gauge value or histogram count, as a number.
    pub value: f64,
}

impl Collector {
    pub fn start() -> Collector {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the collector");
        let endpoint = format!("http://{}", listener.local_addr().expect("address"));
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&received);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let sink = Arc::clone(&sink);
                thread::spawn(move || serve(stream, &sink));
            }
        });
        Collector { endpoint, received }
    }

    /// Every request path received so far, in order.
    pub fn paths(&self) -> Vec<String> {
        let received = self.received.lock().unwrap();
        received.iter().map(|(p, _)| p.clone()).collect()
    }

    /// Every request body received so far, concatenated.
    pub fn raw(&self) -> Vec<u8> {
        let received = self.received.lock().unwrap();
        received.iter().flat_map(|(_, b)| b.clone()).collect()
    }

    pub fn spans(&self) -> Vec<Span> {
        let received = self.received.lock().unwrap();
        let mut out = Vec::new();
        for (path, body) in received.iter() {
            if path != "/v1/traces" {
                continue;
            }
            let req = ExportTraceServiceRequest::decode(body.as_slice()).expect("OTLP traces");
            for rs in req.resource_spans {
                let resource = attrs(&rs.resource.map(|r| r.attributes).unwrap_or_default());
                for ss in rs.scope_spans {
                    for s in ss.spans {
                        out.push(Span {
                            name: s.name,
                            trace_id: s.trace_id,
                            span_id: s.span_id,
                            parent: s.parent_span_id,
                            error: s.status.is_some_and(|st| st.code == 2),
                            attrs: attrs(&s.attributes),
                            resource: resource.clone(),
                        });
                    }
                }
            }
        }
        out
    }

    pub fn points(&self) -> Vec<Point> {
        let received = self.received.lock().unwrap();
        let mut out = Vec::new();
        for (path, body) in received.iter() {
            if path != "/v1/metrics" {
                continue;
            }
            let req = ExportMetricsServiceRequest::decode(body.as_slice()).expect("OTLP metrics");
            for rm in req.resource_metrics {
                for sm in rm.scope_metrics {
                    for m in sm.metrics {
                        let point = |a: &[KeyValue], value: f64| Point {
                            metric: m.name.clone(),
                            unit: m.unit.clone(),
                            attrs: attrs(a),
                            value,
                        };
                        match &m.data {
                            Some(metric::Data::Sum(s)) => out.extend(
                                s.data_points
                                    .iter()
                                    .map(|p| point(&p.attributes, number(p))),
                            ),
                            Some(metric::Data::Gauge(g)) => out.extend(
                                g.data_points
                                    .iter()
                                    .map(|p| point(&p.attributes, number(p))),
                            ),
                            Some(metric::Data::Histogram(h)) => out.extend(
                                h.data_points
                                    .iter()
                                    .map(|p| point(&p.attributes, p.count as f64)),
                            ),
                            _ => {}
                        }
                    }
                }
            }
        }
        out
    }
}

fn number(p: &opentelemetry_proto::tonic::metrics::v1::NumberDataPoint) -> f64 {
    use opentelemetry_proto::tonic::metrics::v1::number_data_point::Value;
    match p.value {
        Some(Value::AsInt(i)) => i as f64,
        Some(Value::AsDouble(d)) => d,
        None => 0.0,
    }
}

fn attrs(kvs: &[KeyValue]) -> Attrs {
    kvs.iter()
        .map(|kv| (kv.key.clone(), value(kv.value.as_ref())))
        .collect()
}

fn value(v: Option<&AnyValue>) -> String {
    match v.and_then(|v| v.value.as_ref()) {
        Some(any_value::Value::StringValue(s)) => s.clone(),
        Some(any_value::Value::IntValue(i)) => i.to_string(),
        Some(any_value::Value::BoolValue(b)) => b.to_string(),
        Some(any_value::Value::DoubleValue(d)) => d.to_string(),
        other => format!("{other:?}"),
    }
}

/// Answer each HTTP/1.1 request on one connection until the client closes it.
fn serve(stream: TcpStream, sink: &Received) {
    let mut writer = stream.try_clone().expect("clone the stream");
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let path = line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let mut length = 0;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                return;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((k, v)) = header.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        sink.lock().unwrap().push((path, body));
        let ok =
            b"HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\ncontent-length: 0\r\n\r\n";
        if writer.write_all(ok).is_err() {
            return;
        }
    }
}
