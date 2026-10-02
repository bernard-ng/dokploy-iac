//! A stateful Dokploy double for the service server-placement suites.
//!
//! The fake keeps a world of external servers and of Compose and database
//! services in memory and implements the `server.all`, per-kind
//! `search`/`one`/`create`/`remove` (and Compose `delete`) endpoints against it.
//! Servers embed credential-shaped canaries in every nested record, the way the
//! real API embeds commands and metrics tokens, so any leak surfaces in the
//! suites. Tests can mutate the world out of band (re-create a server record
//! under the same name, drop a record, make reads fail), inject create faults,
//! and inspect every captured request. Requests that match no endpoint are
//! answered with 501 and reported through `unrouted`.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dokploy_sdk::Dokploy;
use dokploy_state::{
    ExpectedState, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, StateFile, StateStore,
};

/// A credential-shaped canary placed in every nested record the real API embeds.
pub const SECRET_CANARY: &str = "placement-world-secret-canary-never-leak";
/// The fixed fake fingerprint key used instead of the system keychain.
const FINGERPRINT_KEY: &str = "0199a0c8-2351-7c31-8899-2c8f81983ea5:";

const ENVIRONMENTS: &str =
    r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#;
const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
const TOPOLOGY: &str = r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"applications":[],"postgres":[],"redis":[]}]}]"#;

/// Selects the explicit fake fingerprint key so no test touches the keychain.
pub fn use_fake_fingerprint_key() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: the key is set once, before any test in this binary reads it, and
        // never changes afterwards.
        unsafe {
            std::env::set_var(
                "DOKPLOY_FINGERPRINT_KEY",
                format!("{FINGERPRINT_KEY}{}", "07".repeat(32)),
            );
        }
    });
}

/// One Compose or database kind that Dokploy lets a create call place on a server.
#[derive(Clone, Copy)]
pub struct Kind {
    pub name: &'static str,
    pub resource: ResourceKind,
    /// The path prefix of the Dokploy endpoints, such as `postgres` in `postgres.create`.
    pub endpoint: &'static str,
    /// The create-time properties other than `server`, as configuration lines.
    pub body: &'static str,
}

pub const KINDS: [Kind; 7] = [
    Kind {
        name: "compose",
        resource: ResourceKind::Compose,
        endpoint: "compose",
        body: "        document: { file: compose.yaml }\n",
    },
    Kind {
        name: "postgres",
        resource: ResourceKind::Postgres,
        endpoint: "postgres",
        body: "        database: app\n        username: app\n        password: { file: password }\n",
    },
    Kind {
        name: "mysql",
        resource: ResourceKind::MySql,
        endpoint: "mysql",
        body: "        database: app\n        username: app\n        password: { file: password }\n        root_password: { file: root-password }\n",
    },
    Kind {
        name: "mariadb",
        resource: ResourceKind::MariaDb,
        endpoint: "mariadb",
        body: "        database: app\n        username: app\n        password: { file: password }\n        root_password: { file: root-password }\n",
    },
    Kind {
        name: "mongo",
        resource: ResourceKind::Mongo,
        endpoint: "mongo",
        body: "        username: app\n        password: { file: password }\n",
    },
    Kind {
        name: "libsql",
        resource: ResourceKind::LibSql,
        endpoint: "libsql",
        body: "        username: app\n        password: { file: password }\n        node: { type: primary }\n",
    },
    Kind {
        name: "redis",
        resource: ResourceKind::Redis,
        endpoint: "redis",
        body: "        password: { file: password }\n",
    },
];

impl Kind {
    pub fn address(&self) -> ResourceAddress {
        address(&format!("{}.main", self.name))
    }

    pub fn id_key(&self) -> String {
        format!("{}Id", self.endpoint)
    }

    pub fn create_line(&self) -> String {
        format!("POST /api/{}.create", self.endpoint)
    }

    pub fn delete_line(&self) -> String {
        match self.endpoint {
            "compose" => "POST /api/compose.delete".to_owned(),
            other => format!("POST /api/{other}.remove"),
        }
    }

    /// The remote identity the fake assigns to the first service it creates.
    pub fn first_id(&self) -> String {
        format!("{}-1", self.name)
    }

    /// A complete configuration for this kind with the given `server` lines.
    ///
    /// `server` is the full indented block (or empty to leave placement unmanaged).
    pub fn config(&self, server: &str, extra: &str) -> String {
        format!(
            "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      {}:\n        main:\n{}{}{}",
            self.name,
            nest_yaml(self.body),
            nest_yaml(server),
            nest_yaml(extra)
        )
    }

    /// The configuration lines whose values are not secrets (Compose keeps its document).
    pub fn bare_body(&self) -> &'static str {
        match self.name {
            "compose" => self.body,
            "postgres" | "mysql" | "mariadb" => "        database: app\n        username: app\n",
            "mongo" => "        username: app\n",
            "libsql" => "        username: app\n        node: { type: primary }\n",
            _ => "",
        }
    }

    /// The durable inputs that match [`Self::bare_body`].
    pub fn bare_inputs(&self) -> serde_json::Value {
        match self.name {
            "postgres" | "mysql" | "mariadb" => {
                serde_json::json!({"database": "app", "username": "app"})
            }
            "mongo" => serde_json::json!({"username": "app"}),
            "libsql" => serde_json::json!({"username": "app", "node": {"type": "primary"}}),
            _ => serde_json::json!({}),
        }
    }

    /// Like [`Self::bare_config`] but without Compose's document too.
    ///
    /// An unmanaged Compose document is valid only for a protected service, so callers
    /// must add `lifecycle: protect: true` for Compose.
    pub fn quiet_config(&self, server: &str, extra: &str) -> String {
        let body = if self.name == "compose" {
            ""
        } else {
            self.bare_body()
        };
        format!(
            "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      {}:\n        main:\n{}{}{}",
            self.name,
            nest_yaml(body),
            nest_yaml(server),
            nest_yaml(extra)
        )
    }

    /// Like [`Self::config`] without any secret property (except Compose's document).
    pub fn bare_config(&self, server: &str, extra: &str) -> String {
        format!(
            "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      {}:\n        main:\n{}{}{}",
            self.name,
            nest_yaml(self.bare_body()),
            nest_yaml(server),
            extra
        )
    }
}

/// A `server:` block selecting a record by exact name.
pub fn named(name: &str) -> String {
    format!("        server:\n          name: {name}\n")
}

/// A `server:` block selecting Dokploy's own host.
pub const LOCAL: &str = "        server:\n          local: true\n";

/// Writes the secret files every kind's configuration refers to.
pub fn write_secrets(directory: &Path) {
    use std::fs;
    fs::write(directory.join("password"), "placement-password-secret").unwrap();
    fs::write(directory.join("root-password"), "placement-root-secret").unwrap();
    fs::write(directory.join("compose.yaml"), "services: {}\n").unwrap();
}

#[derive(Clone)]
pub struct ServerRow {
    pub id: String,
    pub name: String,
}

/// One Compose or database service served by the world.
#[derive(Clone)]
pub struct Service {
    pub kind: &'static str,
    pub id: String,
    pub name: String,
    /// `None` is Dokploy's local host.
    pub server: Option<String>,
    /// Create-body fields echoed back by reads.
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// How a one-shot fault treats the next matching mutation.
#[derive(Clone, Copy)]
pub enum Mode {
    /// Answer with this HTTP status before applying anything.
    Reject(&'static str),
    /// Apply the mutation, then close the connection without a response.
    DropAfterApply,
    /// Close the connection without applying the mutation.
    DropBeforeApply,
}

pub struct World {
    pub servers: Vec<ServerRow>,
    pub services: Vec<Service>,
    /// Serves a 500 for `server.all`.
    pub servers_unavailable: bool,
    /// Omits `serverId` from every direct read and topology entry.
    pub omit_server_field: bool,
    next_id: BTreeMap<&'static str, u32>,
    faults: Vec<(String, Mode)>,
    log: Vec<String>,
    unrouted: Vec<String>,
}

impl World {
    pub fn fault(&mut self, path: &str, mode: Mode) {
        self.faults.push((path.to_owned(), mode));
    }

    pub fn server_id(&self, name: &str) -> Option<String> {
        self.servers
            .iter()
            .find(|server| server.name == name)
            .map(|server| server.id.clone())
    }

    pub fn add_server(&mut self, id: &str, name: &str) {
        self.servers.push(ServerRow {
            id: id.to_owned(),
            name: name.to_owned(),
        });
    }

    /// Deletes a server record and re-creates it under the same name with a new identity.
    pub fn recreate_server(&mut self, name: &str, new_id: &str) {
        self.servers.retain(|server| server.name != name);
        self.add_server(new_id, name);
    }

    pub fn service(&self, kind: &str, name: &str) -> Option<&Service> {
        self.services
            .iter()
            .find(|service| service.kind == kind && service.name == name)
    }

    pub fn service_mut(&mut self, kind: &str, name: &str) -> &mut Service {
        self.services
            .iter_mut()
            .find(|service| service.kind == kind && service.name == name)
            .expect("service exists")
    }

    pub fn count(&self, kind: &str) -> usize {
        self.services
            .iter()
            .filter(|service| service.kind == kind)
            .count()
    }
}

pub struct Fake {
    pub url: String,
    world: Arc<Mutex<World>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Fake {
    /// Starts a world with two external servers, `edge-1` and `edge-2`.
    pub fn start() -> Self {
        use_fake_fingerprint_key();
        let world = World {
            servers: vec![
                ServerRow {
                    id: "server-1".to_owned(),
                    name: "edge-1".to_owned(),
                },
                ServerRow {
                    id: "server-2".to_owned(),
                    name: "edge-2".to_owned(),
                },
            ],
            services: Vec::new(),
            servers_unavailable: false,
            omit_server_field: false,
            next_id: BTreeMap::new(),
            faults: Vec::new(),
            log: Vec::new(),
            unrouted: Vec::new(),
        };
        let world = Arc::new(Mutex::new(world));
        let listener = TcpListener::bind("127.0.0.1:0").expect("fake binds");
        listener.set_nonblocking(true).expect("fake is nonblocking");
        let address = listener.local_addr().expect("fake has an address");
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (world, stop) = (world.clone(), stop.clone());
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    stream.set_nonblocking(false).expect("connection blocks");
                    serve(&mut stream, &world);
                }
            })
        };

        Self {
            url: format!("http://{address}"),
            world,
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

    pub fn instance(&self) -> InstanceIdentity {
        InstanceIdentity::parse(&self.url).expect("fake URL is an instance")
    }

    pub fn world(&self) -> MutexGuard<'_, World> {
        self.world.lock().expect("world lock")
    }

    pub fn requests(&self) -> Vec<String> {
        self.world().log.clone()
    }

    /// Returns request lines such as `POST /api/postgres.create` in arrival order.
    pub fn lines(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|request| {
                request
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split(" HTTP/")
                    .next()
                    .unwrap_or_default()
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    pub fn matching(&self, prefix: &str) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|request| request.starts_with(prefix))
            .collect()
    }

    pub fn count(&self, line: &str) -> usize {
        self.lines()
            .iter()
            .filter(|candidate| *candidate == line)
            .count()
    }

    pub fn unrouted(&self) -> Vec<String> {
        self.world().unrouted.clone()
    }

    pub fn forget_requests(&self) {
        self.world().log.clear();
    }

    /// Asserts that nothing deploys, starts, or changes an external server.
    pub fn assert_inert(&self) {
        assert!(self.unrouted().is_empty(), "{:?}", self.unrouted());
        for request in self.requests() {
            let line = request.lines().next().unwrap_or_default();
            for forbidden in [
                ".deploy",
                ".redeploy",
                ".start",
                ".reload",
                ".move",
                "server.create",
                "server.update",
                "server.remove",
                "server.setup",
                "testConnection",
            ] {
                assert!(!line.contains(forbidden), "{line}");
            }
        }
    }
}

impl Drop for Fake {
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
        let count = stream.read(&mut buffer).unwrap_or(0);
        bytes.extend_from_slice(&buffer[..count]);
        if count == 0 || request_is_complete(&bytes) {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn request_is_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
    let length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default();
    bytes.len() >= header_end + 4 + length
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn query(line: &str) -> BTreeMap<String, String> {
    let target = line.split(' ').nth(1).unwrap_or_default();
    target
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn serve(stream: &mut TcpStream, world: &Arc<Mutex<World>>) {
    let request = read_request(stream);
    let line = request.lines().next().unwrap_or_default().to_owned();
    let method = line.split(' ').next().unwrap_or_default().to_owned();
    let path = line
        .split(' ')
        .nth(1)
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default()
        .trim_start_matches("/api/")
        .to_owned();
    let body: serde_json::Value = request
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .and_then(|body| serde_json::from_str(body).ok())
        .unwrap_or(serde_json::Value::Null);
    let mut world = world.lock().expect("world lock");
    world.log.push(request);

    let mut mode = None;
    if method == "POST"
        && let Some(index) = world.faults.iter().position(|(fault, _)| *fault == path)
    {
        mode = Some(world.faults.remove(index).1);
    }
    match mode {
        Some(Mode::Reject(status)) => {
            return respond(stream, status, r#"{"message":"rejected"}"#);
        }
        Some(Mode::DropBeforeApply) => return,
        Some(Mode::DropAfterApply) | None => {}
    }

    let (status, reply) = route(&mut world, &method, &path, &line, &body);
    if matches!(mode, Some(Mode::DropAfterApply)) {
        return;
    }
    respond(stream, status, &reply);
}

fn not_found(message: &str) -> (&'static str, String) {
    (
        "404 Not Found",
        serde_json::json!({"code":"NOT_FOUND","message":message}).to_string(),
    )
}

fn route(
    world: &mut World,
    method: &str,
    path: &str,
    line: &str,
    body: &serde_json::Value,
) -> (&'static str, String) {
    let params = query(line);
    match (method, path) {
        ("GET", "project.all") => ("200 OK", TOPOLOGY.to_owned()),
        ("GET", "project.one") => ("200 OK", project_one(world)),
        ("GET", "environment.byProjectId") => ("200 OK", ENVIRONMENTS.to_owned()),
        ("GET", "environment.one") => ("200 OK", ENVIRONMENT.to_owned()),
        ("GET", "server.all") if world.servers_unavailable => (
            "500 Internal Server Error",
            serde_json::json!({"message": SECRET_CANARY}).to_string(),
        ),
        ("GET", "server.all") => {
            let rows = world
                .servers
                .iter()
                .map(|server| {
                    serde_json::json!({
                        "serverId": server.id,
                        "name": server.name,
                        "serverType": "deploy",
                        "ipAddress": "203.0.113.7",
                        "command": format!("curl {SECRET_CANARY}"),
                        "metricsConfig": { "token": SECRET_CANARY },
                        "sshKey": { "privateKey": SECRET_CANARY },
                    })
                })
                .collect::<Vec<_>>();
            ("200 OK", serde_json::Value::Array(rows).to_string())
        }
        ("GET", "mounts.listByServiceId" | "schedule.list") => ("200 OK", "[]".to_owned()),
        ("POST", "project.remove" | "environment.remove") => ("200 OK", "true".to_owned()),
        ("POST", other) if other.ends_with(".create") => {
            create(world, other.trim_end_matches(".create"), body)
        }
        ("POST", other) if other.ends_with(".remove") || other == "compose.delete" => {
            let kind = other
                .trim_end_matches(".remove")
                .trim_end_matches(".delete");
            let id = body[format!("{kind}Id")]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let before = world.services.len();
            world
                .services
                .retain(|service| !(service.kind == kind && service.id == id));
            if world.services.len() == before {
                return not_found("service not found");
            }
            ("200 OK", "true".to_owned())
        }
        ("GET", other) if other.ends_with(".search") => {
            let kind = other.trim_end_matches(".search");
            let items = world
                .services
                .iter()
                .filter(|service| service.kind == kind)
                .map(|service| {
                    serde_json::json!({
                        format!("{kind}Id"): service.id,
                        "environmentId": "environment-1",
                        "name": service.name,
                    })
                })
                .collect::<Vec<_>>();
            let total = items.len();
            (
                "200 OK",
                serde_json::json!({"items": items, "total": total}).to_string(),
            )
        }
        ("GET", other) if other.ends_with(".one") => {
            let kind = other.trim_end_matches(".one");
            let id = params
                .get(&format!("{kind}Id"))
                .cloned()
                .unwrap_or_default();
            match world
                .services
                .iter()
                .find(|service| service.kind == kind && service.id == id)
            {
                Some(service) => ("200 OK", details(world, service).to_string()),
                None => not_found("service not found"),
            }
        }
        _ => {
            world.unrouted.push(line.to_owned());
            (
                "501 Not Implemented",
                r#"{"message":"unrouted"}"#.to_owned(),
            )
        }
    }
}

fn server_field(world: &World, service: &Service) -> Option<serde_json::Value> {
    (!world.omit_server_field).then(|| match &service.server {
        Some(id) => serde_json::Value::String(id.clone()),
        None => serde_json::Value::Null,
    })
}

fn details(world: &World, service: &Service) -> serde_json::Value {
    let kind = service.kind;
    let mut value = serde_json::json!({
        format!("{kind}Id"): service.id,
        "environmentId": "environment-1",
        "name": service.name,
        "appName": service.name,
        "dockerImage": "image:1",
        "sourceType": "raw",
        "composeType": "docker-compose",
        "sqldNode": "primary",
        "sqldPrimaryUrl": null,
        "databasePassword": SECRET_CANARY,
        "databaseRootPassword": SECRET_CANARY,
        "composeFile": SECRET_CANARY,
        "env": SECRET_CANARY,
        "mounts": [{ "content": SECRET_CANARY }],
        "backups": [],
    });
    for (key, field) in &service.fields {
        if !matches!(
            key.as_str(),
            "databasePassword" | "databaseRootPassword" | "composeFile" | "serverId"
        ) {
            value[key] = field.clone();
        }
    }
    if let Some(server) = server_field(world, service) {
        value["serverId"] = server;
    }
    value
}

fn project_one(world: &World) -> String {
    let libsql = world
        .services
        .iter()
        .filter(|service| service.kind == "libsql")
        .map(|service| {
            let mut value = serde_json::json!({
                "libsqlId": service.id,
                "name": service.name,
                "appName": service.name,
                "description": service.fields.get("description").cloned().unwrap_or(serde_json::Value::Null),
            });
            if let Some(server) = server_field(world, service) {
                value["serverId"] = server;
            }
            value
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "projectId": "project-1",
        "name": "platform",
        "environments": [{
            "environmentId": "environment-1",
            "name": "production",
            "isDefault": true,
            "applications": [],
            "postgres": [],
            "redis": [],
            "libsql": libsql,
        }],
    })
    .to_string()
}

fn create(world: &mut World, kind: &str, body: &serde_json::Value) -> (&'static str, String) {
    let Some(known) = KINDS.iter().find(|candidate| candidate.endpoint == kind) else {
        return (
            "400 Bad Request",
            r#"{"message":"unknown kind"}"#.to_owned(),
        );
    };
    let name = body["name"].as_str().unwrap_or_default().to_owned();
    if name.is_empty() || body["environmentId"] != "environment-1" {
        return ("400 Bad Request", r#"{"message":"bad create"}"#.to_owned());
    }
    // Dokploy v0.30.6 declares `serverId` required on `libsql.create` and rejects a
    // body without the key; every other create takes it as optional.
    if known.endpoint == "libsql" && body.get("serverId").is_none() {
        return (
            "400 Bad Request",
            r#"{"message":"serverId required"}"#.to_owned(),
        );
    }
    // The real API treats an omitted or null `serverId` as the local host.
    let server = body["serverId"].as_str().map(str::to_owned);
    if let Some(server) = &server
        && !world
            .servers
            .iter()
            .any(|candidate| &candidate.id == server)
    {
        return (
            "400 Bad Request",
            r#"{"message":"unknown server"}"#.to_owned(),
        );
    }
    let sequence = world.next_id.entry(known.endpoint).or_insert(0);
    *sequence += 1;
    let id = format!("{}-{}", known.name, sequence);
    let mut fields = serde_json::Map::new();
    if let Some(object) = body.as_object() {
        for (key, value) in object {
            if !matches!(
                key.as_str(),
                "name" | "environmentId" | "appName" | "dockerImage"
            ) {
                fields.insert(key.clone(), value.clone());
            }
        }
    }
    let service = Service {
        kind: known.endpoint,
        id: id.clone(),
        name,
        server,
        fields,
    };
    let reply = match known.endpoint {
        "libsql" => "true".to_owned(),
        _ => details(world, &service).to_string(),
    };
    world.services.push(service);
    ("200 OK", reply)
}

/// The resource addresses and physical identities the suites seed into durable state.
pub fn address(value: &str) -> ResourceAddress {
    value.parse().expect("address is valid")
}

/// Seeds project and environment state plus any extra resources.
pub fn seed_workspace(
    directory: &Path,
    instance: InstanceIdentity,
    extra: Vec<(&str, ResourceState)>,
) {
    let store = StateStore::new(directory, instance.clone()).expect("state store");
    let mut state = StateFile::new("0.1.0".parse().expect("version"), instance);
    store
        .begin_write()
        .expect("writer")
        .checkpoint(ExpectedState::absent(), &state)
        .expect("initial checkpoint");
    let mut entries = vec![
        ("project.platform", ResourceKind::Project, "project-1", None),
        (
            "environment.production",
            ResourceKind::Environment,
            "environment-1",
            Some("project.platform"),
        ),
    ]
    .into_iter()
    .map(|(name, kind, id, containment)| {
        (
            address(name),
            ResourceState::new(
                kind,
                RemoteId::new(id).expect("remote id"),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).expect("inputs"),
                containment.map(address),
                Vec::new(),
            ),
        )
    })
    .collect::<Vec<_>>();
    entries.extend(
        extra
            .into_iter()
            .map(|(name, state)| (address(name), state)),
    );
    for (address, resource) in entries {
        let before = state.clone();
        state.upsert_resource(address, resource).expect("upsert");
        store
            .begin_write()
            .expect("writer")
            .checkpoint(ExpectedState::from_state(&before), &state)
            .expect("checkpoint");
    }
}

/// Durable state for one service that manages only its `server` selector.
pub fn placed_state(kind: &Kind, id: &str, server: &str, protected: bool) -> ResourceState {
    let mut inputs = kind.bare_inputs();
    inputs["server"] = serde_json::json!({ "name": server });
    ResourceState::new(
        kind.resource,
        RemoteId::new(id).expect("remote id"),
        protected,
        ManagedInputs::try_from_json(inputs).expect("inputs"),
        Some(address("environment.production")),
        Vec::new(),
    )
}

/// The logical address the suites give a dependent of the given kind.
pub fn dependent_address(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Mount => "mount.data",
        ResourceKind::Schedule => "schedule.nightly",
        ResourceKind::Backup => "backup.nightly",
        other => panic!("{other:?} is not a service dependent"),
    }
}

/// Durable state for a Mount, Schedule, or Backup that depends on `target`.
pub fn dependent_state(kind: ResourceKind, id: &str, target: &str) -> ResourceState {
    let inputs = match kind {
        ResourceKind::Mount => serde_json::json!({
            "target": target,
            "mount_type": "volume",
            "mount_path": "/data",
            "volume_name": "data"
        }),
        ResourceKind::Schedule => serde_json::json!({
            "target": target,
            "service_name": "web",
            "name": "nightly",
            "cron_expression": "0 3 * * *",
            "shell_type": "bash",
            "enabled": false
        }),
        ResourceKind::Backup => serde_json::json!({
            "target": target,
            "destination": { "name": "offsite" },
            "schedule": "0 3 * * *",
            "prefix": "nightly",
            "database": "app",
            "enabled": false,
            "keep_latest": 7,
            "include_encryption_key": false
        }),
        other => panic!("{other:?} is not a service dependent"),
    };
    ResourceState::new(
        kind,
        RemoteId::new(id).expect("remote id"),
        false,
        ManagedInputs::try_from_json(inputs).expect("inputs"),
        Some(address("environment.production")),
        vec![address(target)],
    )
}

/// The in-memory state matching [`seed_workspace`] with the given services.
pub fn state_with(fake: &Fake, extra: Vec<(&str, ResourceState)>) -> StateFile {
    let directory = tempfile::tempdir().expect("temporary workspace");
    seed_workspace(directory.path(), fake.instance(), extra);
    StateStore::new(directory.path(), fake.instance())
        .expect("state store")
        .inspect()
        .expect("state reads")
        .expect("state exists")
}

/// Indents a YAML fragment so it nests one level deeper under `project`.
pub fn nest_yaml(fragment: &str) -> String {
    fragment.lines().map(|line| format!("  {line}\n")).collect()
}
