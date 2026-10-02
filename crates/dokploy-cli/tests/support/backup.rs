//! A small stateful Dokploy double for the Backup suites.
//!
//! Unlike the path-routed `Router`, this fake keeps a world of database
//! targets, backup destinations, and Backups in memory and implements the
//! `backup.*`, `destination.all`, and database `*.one`/`*.search` endpoints
//! against it. Lifecycle tests therefore never depend on request ordering, and
//! tests can mutate the world out of band (drift), inject faults, and inspect
//! every captured request. Requests that match no endpoint are answered with
//! 501 and reported through `unrouted`.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dokploy_sdk::Dokploy;
use dokploy_state::{
    ExpectedState, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind,
    ResourceState, StateFile, StateStore,
};

/// A credential-shaped canary placed in every nested record the real API embeds.
pub const SECRET_CANARY: &str = "backup-world-secret-canary-never-leak";

pub const ENVIRONMENTS: &str =
    r#"[{"environmentId":"environment-1","name":"production","projectId":"project-1"}]"#;
pub const ENVIRONMENT: &str =
    r#"{"environmentId":"environment-1","name":"production","projectId":"project-1"}"#;
const TOPOLOGY: &str = r#"[{"projectId":"project-1","name":"platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"applications":[],"postgres":[],"redis":[]}]}]"#;

/// One database target served by the world.
#[derive(Clone)]
pub struct Target {
    pub kind: &'static str,
    pub id: String,
    pub name: String,
}

/// One Backup row.
#[derive(Clone)]
pub struct Row {
    pub id: String,
    pub kind: String,
    pub target_id: String,
    pub destination_id: String,
    pub schedule: String,
    pub enabled: Option<bool>,
    pub prefix: String,
    pub database: String,
    pub keep: Option<u32>,
    pub include_key: bool,
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
    pub targets: Vec<Target>,
    pub destinations: Vec<(String, String)>,
    pub rows: Vec<Row>,
    /// Hides every row from the target collections while keeping direct reads.
    pub hide_from_collections: bool,
    /// Serves a 404 for direct reads of this Backup while collections still list it.
    pub direct_404: Vec<String>,
    /// Makes target collections disagree with direct reads about the schedule.
    pub collection_skew: bool,
    /// Serves a 500 for `destination.all`.
    pub destinations_unavailable: bool,
    next_id: u32,
    faults: Vec<(&'static str, Mode)>,
    log: Vec<String>,
    unrouted: Vec<String>,
}

impl World {
    pub fn fault(&mut self, path: &'static str, mode: Mode) {
        self.faults.push((path, mode));
    }

    pub fn add_row(&mut self, row: Row) {
        self.rows.push(row);
    }

    pub fn destination_id(&self, name: &str) -> Option<String> {
        self.destinations
            .iter()
            .find(|(_, candidate)| candidate == name)
            .map(|(id, _)| id.clone())
    }

    pub fn row_mut(&mut self, id: &str) -> &mut Row {
        self.rows
            .iter_mut()
            .find(|row| row.id == id)
            .expect("row exists")
    }
}

pub struct Fake {
    pub url: String,
    world: Arc<Mutex<World>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// A row for a Postgres target with the defaults the suites use.
pub fn row(id: &str, target_id: &str, destination_id: &str, prefix: &str) -> Row {
    Row {
        id: id.to_owned(),
        kind: "postgres".to_owned(),
        target_id: target_id.to_owned(),
        destination_id: destination_id.to_owned(),
        schedule: "0 3 * * *".to_owned(),
        enabled: Some(false),
        prefix: prefix.to_owned(),
        database: "app".to_owned(),
        keep: Some(7),
        include_key: false,
    }
}

impl Fake {
    /// Starts a world with Postgres `main`, MySQL `sql`, and two destinations.
    pub fn start() -> Self {
        let world = World {
            targets: vec![
                Target {
                    kind: "postgres",
                    id: "postgres-1".to_owned(),
                    name: "main".to_owned(),
                },
                Target {
                    kind: "mysql",
                    id: "mysql-1".to_owned(),
                    name: "sql".to_owned(),
                },
                Target {
                    kind: "postgres",
                    id: "postgres-2".to_owned(),
                    name: "other".to_owned(),
                },
            ],
            destinations: vec![
                ("destination-1".to_owned(), "offsite".to_owned()),
                ("destination-2".to_owned(), "archive".to_owned()),
            ],
            rows: Vec::new(),
            hide_from_collections: false,
            direct_404: Vec::new(),
            collection_skew: false,
            destinations_unavailable: false,
            next_id: 1,
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

    /// Returns request lines such as `POST /api/backup.create` in arrival order.
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

    /// Asserts that nothing deploys, runs a backup, or contacts a destination.
    pub fn assert_inert(&self) {
        assert!(self.unrouted().is_empty(), "{:?}", self.unrouted());
        for request in self.requests() {
            let line = request.lines().next().unwrap_or_default();
            for forbidden in [
                ".deploy",
                ".redeploy",
                "manualBackup",
                "testConnection",
                "restore",
                ".start",
                ".reload",
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
        ("GET", "project.one") => (
            "200 OK",
            r#"{"projectId":"project-1","name":"platform","environments":[]}"#.to_owned(),
        ),
        ("GET", "environment.byProjectId") => ("200 OK", ENVIRONMENTS.to_owned()),
        ("GET", "environment.one") => ("200 OK", ENVIRONMENT.to_owned()),
        ("GET", "destination.all") if world.destinations_unavailable => (
            "500 Internal Server Error",
            r#"{"message":"unavailable"}"#.to_owned(),
        ),
        ("GET", "destination.all") => {
            let rows = world
                .destinations
                .iter()
                .map(|(id, name)| {
                    serde_json::json!({
                        "destinationId": id,
                        "name": name,
                        "provider": "S3",
                        "accessKey": SECRET_CANARY,
                        "secretAccessKey": SECRET_CANARY,
                        "bucket": SECRET_CANARY,
                        "region": "r",
                        "endpoint": SECRET_CANARY,
                    })
                })
                .collect::<Vec<_>>();
            ("200 OK", serde_json::Value::Array(rows).to_string())
        }
        ("GET", "backup.one") => {
            let id = params.get("backupId").cloned().unwrap_or_default();
            if world.direct_404.contains(&id) {
                return not_found("Backup not found");
            }
            match world.rows.iter().find(|row| row.id == id) {
                Some(row) => ("200 OK", record(row).to_string()),
                None => not_found("Backup not found"),
            }
        }
        ("POST", "backup.create") => create(world, body),
        ("POST", "backup.update") => update(world, body),
        ("POST", "backup.remove") => {
            let id = body["backupId"].as_str().unwrap_or_default().to_owned();
            let before = world.rows.len();
            world.rows.retain(|row| row.id != id);
            if world.rows.len() == before {
                return not_found("Backup not found");
            }
            ("200 OK", "true".to_owned())
        }
        ("POST", "project.remove" | "environment.remove") => ("200 OK", "true".to_owned()),
        ("POST", other) if other.ends_with(".remove") => {
            let kind = other.trim_end_matches(".remove");
            let id = body[format!("{kind}Id")]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            world
                .targets
                .retain(|target| !(target.kind == kind && target.id == id));
            world.rows.retain(|row| row.target_id != id);
            ("200 OK", "true".to_owned())
        }
        ("GET", other) if other.ends_with(".search") => {
            let kind = other.trim_end_matches(".search");
            let items = world
                .targets
                .iter()
                .filter(|target| target.kind == kind)
                .map(|target| {
                    serde_json::json!({
                        format!("{kind}Id"): target.id,
                        "environmentId": "environment-1",
                        "name": target.name,
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
                .targets
                .iter()
                .find(|target| target.kind == kind && target.id == id)
            {
                Some(target) => {
                    let backups = if world.hide_from_collections {
                        Vec::new()
                    } else {
                        world
                            .rows
                            .iter()
                            .filter(|row| row.target_id == id && row.kind == kind)
                            .map(|row| {
                                let mut value = record(row);
                                if world.collection_skew {
                                    value["schedule"] = serde_json::json!("1 1 1 1 1");
                                }
                                value
                            })
                            .collect()
                    };
                    (
                        "200 OK",
                        serde_json::json!({
                            format!("{kind}Id"): target.id,
                            "environmentId": "environment-1",
                            "name": target.name,
                            "appName": target.name,
                            "dockerImage": "image:1",
                            "databaseName": "app",
                            "databaseUser": "app",
                            "databasePassword": SECRET_CANARY,
                            "databaseRootPassword": SECRET_CANARY,
                            "env": SECRET_CANARY,
                            "backups": backups,
                        })
                        .to_string(),
                    )
                }
                None => not_found("target not found"),
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

fn record(row: &Row) -> serde_json::Value {
    let mut value = serde_json::json!({
        "backupId": row.id,
        "schedule": row.schedule,
        "enabled": row.enabled,
        "database": row.database,
        "prefix": row.prefix,
        "destinationId": row.destination_id,
        "keepLatestCount": row.keep,
        "includeEncryptionKey": row.include_key,
        "backupType": "database",
        "databaseType": row.kind,
        "composeId": null,
        "postgresId": null,
        "mysqlId": null,
        "mariadbId": null,
        "mongoId": null,
        "libsqlId": null,
        "serviceName": null,
        "metadata": null,
        "destination": { "accessKey": SECRET_CANARY, "secretAccessKey": SECRET_CANARY },
        "deployments": [{ "errorMessage": SECRET_CANARY }],
    });
    value[format!("{}Id", row.kind)] = serde_json::Value::String(row.target_id.clone());
    value
}

fn target_of(body: &serde_json::Value) -> Option<(String, String)> {
    ["postgres", "mysql", "mariadb", "mongo", "libsql"]
        .iter()
        .find_map(|kind| {
            body[format!("{kind}Id")]
                .as_str()
                .map(|id| ((*kind).to_owned(), id.to_owned()))
        })
}

fn create(world: &mut World, body: &serde_json::Value) -> (&'static str, String) {
    let Some((kind, target_id)) = target_of(body) else {
        return ("400 Bad Request", r#"{"message":"no target"}"#.to_owned());
    };
    if body["backupType"] != "database" || body["databaseType"] != kind.as_str() {
        return ("400 Bad Request", r#"{"message":"bad type"}"#.to_owned());
    }
    let id = format!("backup-{}", world.next_id);
    world.next_id += 1;
    world.rows.push(Row {
        id,
        kind,
        target_id,
        destination_id: body["destinationId"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        schedule: body["schedule"].as_str().unwrap_or_default().to_owned(),
        enabled: body["enabled"].as_bool(),
        prefix: body["prefix"].as_str().unwrap_or_default().to_owned(),
        database: body["database"].as_str().unwrap_or_default().to_owned(),
        keep: body["keepLatestCount"]
            .as_u64()
            .and_then(|count| u32::try_from(count).ok()),
        include_key: body["includeEncryptionKey"].as_bool().unwrap_or(false),
    });
    ("200 OK", "true".to_owned())
}

fn update(world: &mut World, body: &serde_json::Value) -> (&'static str, String) {
    let id = body["backupId"].as_str().unwrap_or_default().to_owned();
    let Some(row) = world.rows.iter_mut().find(|row| row.id == id) else {
        return not_found("Backup not found");
    };
    row.destination_id = body["destinationId"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    row.schedule = body["schedule"].as_str().unwrap_or_default().to_owned();
    row.enabled = body["enabled"].as_bool();
    row.prefix = body["prefix"].as_str().unwrap_or_default().to_owned();
    row.database = body["database"].as_str().unwrap_or_default().to_owned();
    row.keep = body["keepLatestCount"]
        .as_u64()
        .and_then(|count| u32::try_from(count).ok());
    row.include_key = body["includeEncryptionKey"].as_bool().unwrap_or(false);
    ("200 OK", "true".to_owned())
}

/// The resource addresses and physical identities the suites seed into durable state.
pub fn address(value: &str) -> ResourceAddress {
    value.parse().expect("address is valid")
}

/// Seeds project, environment, Postgres `main`, MySQL `sql`, and Postgres `other` state.
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
        (
            "postgres.main",
            ResourceKind::Postgres,
            "postgres-1",
            Some("environment.production"),
        ),
        (
            "mysql.sql",
            ResourceKind::MySql,
            "mysql-1",
            Some("environment.production"),
        ),
        (
            "postgres.other",
            ResourceKind::Postgres,
            "postgres-2",
            Some("environment.production"),
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

/// Durable state for one Backup under Postgres or MySQL target `target`.
pub fn backup_state(
    id: &str,
    target: &str,
    destination: &str,
    prefix: &str,
    protected: bool,
) -> ResourceState {
    ResourceState::new(
        ResourceKind::Backup,
        RemoteId::new(id).expect("remote id"),
        protected,
        ManagedInputs::try_from_json(serde_json::json!({
            "target": target,
            "destination": { "name": destination },
            "schedule": "0 3 * * *",
            "prefix": prefix,
            "database": "app",
            "enabled": false,
            "keep_latest": 7,
            "include_encryption_key": false
        }))
        .expect("inputs"),
        Some(address("environment.production")),
        vec![address(target)],
    )
}

/// A configuration with the standard database targets and the given backups.
pub fn config(backups: &str) -> String {
    let backups = nest_yaml(backups);
    format!(
        "version: 1\nproject:\n  name: platform\n  environments:\n    production:\n      postgres:\n        main: {{}}\n        other: {{}}\n      mysql:\n        sql: {{}}\n      backups:\n{backups}"
    )
}

/// The standard disabled Backup configuration under `postgres.main`.
pub const NIGHTLY: &str = "      nightly:\n        target: postgres.main\n        destination: { name: offsite }\n        schedule: \"0 3 * * *\"\n        prefix: nightly\n        database: app\n        enabled: false\n        keep_latest: 7\n";

/// Indents a YAML fragment so it nests one level deeper under `project`.
pub fn nest_yaml(fragment: &str) -> String {
    fragment.lines().map(|line| format!("  {line}\n")).collect()
}
