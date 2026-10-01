//! Path-routed Dokploy test double shared by the Mount suites.
//!
//! Responses are selected by request method, path, and optional query
//! fragments rather than by global order, so discovery ordering never couples a
//! test to unrelated reads. Every request is captured with its body. Requests
//! that match no route are answered with 501 and reported through `unrouted`.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dokploy_sdk::Dokploy;

#[derive(Clone)]
pub enum Reply {
    Json(&'static str, String),
    /// Close the connection without responding, simulating an unknown outcome.
    Drop,
}

pub fn ok(body: impl Into<String>) -> Reply {
    Reply::Json("200 OK", body.into())
}

pub fn status(status: &'static str, body: impl Into<String>) -> Reply {
    Reply::Json(status, body.into())
}

struct Route {
    method_and_path: String,
    fragments: Vec<String>,
    replies: Vec<Reply>,
    next: usize,
}

pub struct Router {
    pub url: String,
    log: Arc<Mutex<Vec<String>>>,
    unrouted: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Router {
    /// Starts a router. A route key is `"METHOD /api/path"` optionally followed
    /// by `|fragment` entries that must all appear in the request line.
    pub fn start(routes: Vec<(&str, Vec<Reply>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        listener
            .set_nonblocking(true)
            .expect("test server is nonblocking");
        let address = listener.local_addr().expect("test server has an address");
        let mut routes = routes
            .into_iter()
            .map(|(key, replies)| {
                let mut parts = key.split('|');
                Route {
                    method_and_path: parts.next().expect("route has a path").to_owned(),
                    fragments: parts.map(str::to_owned).collect(),
                    replies,
                    next: 0,
                }
            })
            .collect::<Vec<_>>();
        // Prefer the most specific route.
        routes.sort_by_key(|route| std::cmp::Reverse(route.fragments.len()));
        let log = Arc::new(Mutex::new(Vec::new()));
        let unrouted = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (log, unrouted, stop) = (log.clone(), unrouted.clone(), stop.clone());
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    stream
                        .set_nonblocking(false)
                        .expect("connection is blocking");
                    let request = read_request(&mut stream);
                    let line = request.lines().next().unwrap_or_default().to_owned();
                    log.lock().unwrap().push(request);
                    let route = routes.iter_mut().find(|route| {
                        line.starts_with(&route.method_and_path)
                            && route
                                .fragments
                                .iter()
                                .all(|fragment| line.contains(fragment))
                    });
                    let reply = match route {
                        Some(route) if !route.replies.is_empty() => {
                            let index = route.next.min(route.replies.len() - 1);
                            route.next += 1;
                            route.replies[index].clone()
                        }
                        _ => {
                            unrouted.lock().unwrap().push(line);
                            Reply::Json("501 Not Implemented", r#"{"message":"unrouted"}"#.into())
                        }
                    };
                    match reply {
                        Reply::Json(status, body) => {
                            let _ = write!(
                                stream,
                                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            );
                        }
                        Reply::Drop => drop(stream),
                    }
                }
            })
        };

        Self {
            url: format!("http://{address}"),
            log,
            unrouted,
            stop,
            thread: Some(thread),
        }
    }

    pub fn client(&self) -> Dokploy {
        Dokploy::builder()
            .url(&self.url)
            .api_key("test-api-key")
            .build()
            .expect("client configuration is valid")
    }

    /// Returns every captured request, including headers and body.
    pub fn requests(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// Returns captured requests whose request line starts with `prefix`.
    pub fn matching(&self, prefix: &str) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|request| request.starts_with(prefix))
            .collect()
    }

    pub fn unrouted(&self) -> Vec<String> {
        self.unrouted.lock().unwrap().clone()
    }

    /// Returns the request-line prefixes in arrival order.
    pub fn lines(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .map(|request| {
                request
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split(" HTTP/")
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }
}

impl Drop for Router {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let count = stream.read(&mut buffer).expect("request is readable");
        bytes.extend_from_slice(&buffer[..count]);
        if count == 0 || request_is_complete(&bytes) {
            break;
        }
    }
    String::from_utf8(bytes).expect("request is UTF-8")
}

fn request_is_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default();
    bytes.len() >= header_end + 4 + content_length
}

pub const PROJECT_CREATE: &str =
    include_str!("../../../../fixtures/api/live/v0.30.6/project-create.owner.json");
pub const APPLICATION_CREATE: &str =
    include_str!("../../../../fixtures/api/live/v0.30.6/application-create.owner.json");

pub const ENVIRONMENTS: &str =
    r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#;
pub const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;

/// Project topology with the default environment and the given application items.
pub fn project_topology(applications: &str) -> String {
    format!(
        r#"[{{"projectId":"project-1","name":"platform","environments":[{{"environmentId":"environment-1","name":"production","isDefault":true,"applications":[{applications}],"postgres":[],"redis":[]}}]}}]"#
    )
}

pub fn application_item(id: &str, name: &str) -> String {
    format!(r#"{{"applicationId":"{id}","environmentId":"environment-1","name":"{name}"}}"#)
}

pub fn application_search(items: &[(&str, &str)]) -> String {
    let items = items
        .iter()
        .map(|(id, name)| application_item(id, name))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"items":[{items}],"total":{}}}"#,
        items.matches("applicationId").count()
    )
}

pub fn application_one(id: &str, name: &str) -> String {
    format!(
        r#"{{"applicationId":"{id}","environmentId":"environment-1","name":"{name}","appName":"{name}"}}"#
    )
}

/// One volume Mount record as returned by `mounts.one` and the target list.
pub fn volume_mount(id: &str, application: &str, path: &str, volume: &str) -> String {
    format!(
        r#"{{"mountId":"{id}","type":"volume","mountPath":"{path}","serviceType":"application","applicationId":"{application}","volumeName":"{volume}","hostPath":null,"filePath":null,"content":null}}"#
    )
}

pub fn file_mount(id: &str, application: &str, path: &str, file: &str) -> String {
    format!(
        r#"{{"mountId":"{id}","type":"file","mountPath":"{path}","serviceType":"application","applicationId":"{application}","volumeName":null,"hostPath":null,"filePath":"{file}","content":"remote-content-is-never-modeled"}}"#
    )
}

pub fn bind_mount(id: &str, application: &str, path: &str, host: &str) -> String {
    format!(
        r#"{{"mountId":"{id}","type":"bind","mountPath":"{path}","serviceType":"application","applicationId":"{application}","volumeName":null,"hostPath":"{host}","filePath":null}}"#
    )
}

pub fn list<S: AsRef<str>>(mounts: impl IntoIterator<Item = S>) -> String {
    let mounts = mounts
        .into_iter()
        .map(|mount| mount.as_ref().to_owned())
        .collect::<Vec<_>>();
    format!("[{}]", mounts.join(","))
}
