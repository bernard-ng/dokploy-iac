use std::io::{Read, Write};
use std::net::TcpListener;
use std::num::NonZeroU32;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    BackupId, BackupTarget, CreateBackup, DestinationId, Dokploy, Error, LibSqlId, MariaDbId,
    MongoId, MySqlId, PostgresId, UpdateBackup,
};

const DESTINATIONS: &str = r#"[{"destinationId":"destination-1","name":"inert"},{"destinationId":"destination-2","name":"inert-updated"}]"#;
const SECRET_CANARY: &str = "backup-secret-canary-do-not-leak";
const BACKUP_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/backup-one.created.owner.json");
const BACKUP_PARENT_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/postgres-one.backup-created.owner.json");

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_in_sequence(responses: Vec<(&'static str, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("test server accepts request");
                received.push(String::from_utf8(read_request(&mut stream)).expect("UTF-8 request"));
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response is writable");
            }
            sender.send(received).expect("test receives requests");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn close_after(responses: Vec<(&'static str, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("test server accepts request");
                received.push(String::from_utf8(read_request(&mut stream)).expect("UTF-8 request"));
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response is writable");
            }
            let (mut stream, _) = listener.accept().expect("test server accepts request");
            received.push(String::from_utf8(read_request(&mut stream)).expect("UTF-8 request"));
            drop(stream);
            sender.send(received).expect("test receives requests");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().expect("test receives requests");
        self.thread.join().expect("test server exits");
        requests
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let count = stream.read(&mut buffer).expect("request is readable");
        bytes.extend_from_slice(&buffer[..count]);
        if count == 0 || request_is_complete(&bytes) {
            break;
        }
    }
    bytes
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

fn client(server: &TestServer) -> Dokploy {
    Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .unwrap()
}

fn leak(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn target(kind: &str, id: &str) -> BackupTarget {
    match kind {
        "postgres" => BackupTarget::Postgres(PostgresId::new(id)),
        "mysql" => BackupTarget::MySql(MySqlId::new(id)),
        "mariadb" => BackupTarget::MariaDb(MariaDbId::new(id)),
        "mongo" => BackupTarget::Mongo(MongoId::new(id)),
        "libsql" => BackupTarget::LibSql(LibSqlId::new(id)),
        _ => panic!("unsupported test target"),
    }
}

fn backup_response(
    kind: &str,
    target_id: &str,
    backup_id: &str,
    destination_id: &str,
    prefix: &str,
    database: &str,
    keep_latest_count: Option<u32>,
) -> String {
    let mut value = serde_json::json!({
        "backupId": backup_id,
        "schedule": "* * * * *",
        "enabled": false,
        "database": database,
        "prefix": prefix,
        "destinationId": destination_id,
        "keepLatestCount": keep_latest_count,
        "includeEncryptionKey": true,
        "backupType": "database",
        "databaseType": kind,
        "composeId": null,
        "postgresId": null,
        "mysqlId": null,
        "mariadbId": null,
        "mongoId": null,
        "libsqlId": null,
        "serviceName": null,
        "metadata": null,
        "destination": {
            "accessKey": SECRET_CANARY,
            "secretAccessKey": SECRET_CANARY
        },
        "postgres": { "databasePassword": SECRET_CANARY },
        "deployments": [{ "errorMessage": SECRET_CANARY }]
    });
    value[format!("{kind}Id")] = serde_json::Value::String(target_id.to_owned());
    serde_json::to_string(&value).unwrap()
}

fn parent_response(kind: &str, target_id: &str, backups: &[String]) -> &'static str {
    leak(
        serde_json::json!({
            format!("{kind}Id"): target_id,
            "databasePassword": SECRET_CANARY,
            "databaseRootPassword": SECRET_CANARY,
            "env": SECRET_CANARY,
            "backups": backups
                .iter()
                .map(|backup| serde_json::from_str::<serde_json::Value>(backup).unwrap())
                .collect::<Vec<_>>()
        })
        .to_string(),
    )
}

fn create_input(target: BackupTarget) -> CreateBackup {
    CreateBackup::new(
        target,
        DestinationId::new("destination-1"),
        "* * * * *",
        false,
        "/initial/",
        "database-initial",
        Some(NonZeroU32::new(2).unwrap()),
        true,
    )
}

fn update_input(target: BackupTarget) -> UpdateBackup {
    UpdateBackup::new(
        BackupId::new("backup-1"),
        target,
        DestinationId::new("destination-2"),
        "* * * * *",
        false,
        "/updated/",
        "database-updated",
        Some(NonZeroU32::new(3).unwrap()),
        false,
    )
}

fn request_body(request: &str) -> serde_json::Value {
    let body = request.split_once("\r\n\r\n").unwrap().1;
    serde_json::from_str(body).unwrap()
}

#[tokio::test]
async fn captured_v0306_backup_fixtures_decode_through_safe_public_models() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", BACKUP_FIXTURE),
        ("200 OK", DESTINATIONS),
        ("200 OK", BACKUP_PARENT_FIXTURE),
        ("200 OK", BACKUP_PARENT_FIXTURE),
    ]);
    let client = client(&server);

    let direct = client
        .backups()
        .get(BackupId::new("backup-1"))
        .await
        .expect("captured direct fixture decodes");
    let collection = client
        .backups()
        .by_target(BackupTarget::Postgres(PostgresId::new("postgres-1")))
        .await
        .expect("captured authoritative fixture decodes");

    assert_eq!(direct.backup_id.as_str(), "backup-1");
    assert_eq!(direct.enabled, Some(false));
    assert_eq!(direct.keep_latest_count.map(NonZeroU32::get), Some(2));
    assert_eq!(collection.backups(), [direct]);
    assert!(!format!("{collection:?}").contains("password"));
    assert!(!format!("{collection:?}").contains("accessKey"));
    assert_eq!(server.finish().len(), 4);
}

#[tokio::test]
async fn every_supported_target_reads_its_authoritative_collection() {
    for kind in ["postgres", "mysql", "mariadb", "mongo", "libsql"] {
        let target_id = format!("{kind}-1");
        let backup = backup_response(
            kind,
            &target_id,
            "backup-1",
            "destination-1",
            "/initial/",
            "database-initial",
            Some(2),
        );
        let server = TestServer::respond_in_sequence(vec![(
            "200 OK",
            parent_response(kind, &target_id, &[backup]),
        )]);

        let collection = client(&server)
            .backups()
            .by_target(target(kind, &target_id))
            .await
            .expect("supported target decodes");

        assert_eq!(collection.backups().len(), 1);
        assert_eq!(collection.backups()[0].backup_id.as_str(), "backup-1");
        let debug = format!("{collection:?}");
        assert!(!debug.contains(SECRET_CANARY));
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with(&format!(
            "GET /api/{kind}.one?{kind}Id={kind}-1 HTTP/1.1\r\n"
        )));
    }
}

#[tokio::test]
async fn create_uses_exact_integer_wire_body_and_one_new_identity_proof() {
    let before = parent_response("postgres", "postgres-1", &[]);
    let created = backup_response(
        "postgres",
        "postgres-1",
        "backup-1",
        "destination-1",
        "/initial/",
        "database-initial",
        Some(2),
    );
    let after = parent_response("postgres", "postgres-1", &[created]);
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", DESTINATIONS),
        ("200 OK", before),
        ("200 OK", "{}"),
        ("200 OK", after),
    ]);

    let created = client(&server)
        .backups()
        .create(create_input(target("postgres", "postgres-1")))
        .await
        .expect("create is proven");

    assert_eq!(created.backup_id().as_str(), "backup-1");
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert!(requests[2].starts_with("POST /api/backup.create HTTP/1.1\r\n"));
    let body = request_body(&requests[2]);
    assert_eq!(body["keepLatestCount"], 2);
    assert!(body["keepLatestCount"].is_u64());
    assert_eq!(body["backupType"], "database");
    assert_eq!(body["databaseType"], "postgres");
    assert_eq!(body["postgresId"], "postgres-1");
    assert_eq!(body["serviceName"], serde_json::Value::Null);
    assert_eq!(body["metadata"], serde_json::Value::Null);
    for absent in ["mysqlId", "mariadbId", "mongoId", "libsqlId", "composeId"] {
        assert!(body.get(absent).is_none());
    }
}

#[tokio::test]
async fn update_proves_target_collision_and_direct_collection_agreement() {
    let existing = leak(backup_response(
        "mysql",
        "mysql-1",
        "backup-1",
        "destination-1",
        "/initial/",
        "database-initial",
        Some(2),
    ));
    let before = parent_response("mysql", "mysql-1", &[existing.to_owned()]);
    let updated = leak(
        backup_response(
            "mysql",
            "mysql-1",
            "backup-1",
            "destination-2",
            "/updated/",
            "database-updated",
            Some(3),
        )
        .replace(
            "\"includeEncryptionKey\":true",
            "\"includeEncryptionKey\":false",
        ),
    );
    let after = parent_response("mysql", "mysql-1", &[updated.to_owned()]);
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", DESTINATIONS),
        ("200 OK", existing),
        ("200 OK", DESTINATIONS),
        ("200 OK", before),
        ("200 OK", "{}"),
        ("200 OK", updated),
        ("200 OK", DESTINATIONS),
        ("200 OK", after),
    ]);

    client(&server)
        .backups()
        .update(update_input(target("mysql", "mysql-1")))
        .await
        .expect("update is proven");

    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert!(requests[4].starts_with("POST /api/backup.update HTTP/1.1\r\n"));
    let body = request_body(&requests[4]);
    assert_eq!(body["backupId"], "backup-1");
    assert_eq!(body["destinationId"], "destination-2");
    assert_eq!(body["databaseType"], "mysql");
    assert!(body.get("mysqlId").is_none());
}

#[tokio::test]
async fn delete_requires_target_and_authoritative_absence() {
    let existing = leak(backup_response(
        "mongo",
        "mongo-1",
        "backup-1",
        "destination-1",
        "/initial/",
        "database-initial",
        Some(2),
    ));
    let before = parent_response("mongo", "mongo-1", &[existing.to_owned()]);
    let after = parent_response("mongo", "mongo-1", &[]);
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", existing),
        ("200 OK", DESTINATIONS),
        ("200 OK", before),
        ("200 OK", "{}"),
        ("200 OK", after),
    ]);

    client(&server)
        .backups()
        .delete(BackupId::new("backup-1"), target("mongo", "mongo-1"))
        .await
        .expect("delete is proven");

    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert!(requests[3].starts_with("POST /api/backup.remove HTTP/1.1\r\n"));
    assert_eq!(
        request_body(&requests[3]),
        serde_json::json!({"backupId":"backup-1"})
    );
}

#[tokio::test]
async fn target_collections_fail_closed_on_unsupported_metadata_and_duplicate_runtime_prefixes() {
    let mut unsupported = serde_json::from_str::<serde_json::Value>(&backup_response(
        "libsql",
        "libsql-1",
        "backup-1",
        "destination-1",
        "/one/",
        "database",
        Some(2),
    ))
    .unwrap();
    unsupported["metadata"] = serde_json::json!({"mysql":{"databaseRootPassword":SECRET_CANARY}});
    let unsupported = unsupported.to_string();
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        parent_response("libsql", "libsql-1", &[unsupported]),
    )]);
    let error = client(&server)
        .backups()
        .by_target(target("libsql", "libsql-1"))
        .await
        .expect_err("metadata is unsupported");
    assert!(!format!("{error:?}").contains(SECRET_CANARY));
    server.finish();

    let first = backup_response(
        "postgres",
        "postgres-1",
        "backup-1",
        "destination-1",
        " /same/ ",
        "database",
        Some(2),
    );
    let second = backup_response(
        "postgres",
        "postgres-1",
        "backup-2",
        "destination-1",
        "same",
        "database",
        Some(2),
    );
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        parent_response("postgres", "postgres-1", &[first, second]),
    )]);
    assert!(matches!(
        client(&server)
            .backups()
            .by_target(target("postgres", "postgres-1"))
            .await,
        Err(Error::UnexpectedResponse { .. })
    ));
    server.finish();
}

#[tokio::test]
async fn create_postflight_failures_are_outcome_unknown_and_never_retried() {
    let before = parent_response("postgres", "postgres-1", &[]);
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", DESTINATIONS),
        ("200 OK", before),
        ("200 OK", "{}"),
        ("500 Internal Server Error", r#"{"message":"private"}"#),
    ]);
    assert!(matches!(
        client(&server)
            .backups()
            .create(create_input(target("postgres", "postgres-1")))
            .await,
        Err(Error::OutcomeUnknown {
            operation: "backup.create",
            ..
        })
    ));
    assert_eq!(server.finish().len(), 4);

    let before = parent_response("postgres", "postgres-1", &[]);
    let server = TestServer::close_after(vec![
        ("200 OK", DESTINATIONS),
        ("200 OK", before),
        ("200 OK", "{}"),
    ]);
    assert!(matches!(
        client(&server)
            .backups()
            .create(create_input(target("postgres", "postgres-1")))
            .await,
        Err(Error::OutcomeUnknown {
            operation: "backup.create",
            ..
        })
    ));
    assert_eq!(server.finish().len(), 4);
}

#[tokio::test]
async fn ambiguous_create_and_update_or_delete_postflight_failures_are_outcome_unknown() {
    let first = backup_response(
        "postgres",
        "postgres-1",
        "backup-1",
        "destination-1",
        "/initial/",
        "database-initial",
        Some(2),
    );
    let second = backup_response(
        "postgres",
        "postgres-1",
        "backup-2",
        "destination-1",
        "/other/",
        "database-other",
        Some(2),
    );
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", DESTINATIONS),
        ("200 OK", parent_response("postgres", "postgres-1", &[])),
        ("200 OK", "{}"),
        (
            "200 OK",
            parent_response("postgres", "postgres-1", &[first, second]),
        ),
    ]);
    assert!(matches!(
        client(&server)
            .backups()
            .create(create_input(target("postgres", "postgres-1")))
            .await,
        Err(Error::OutcomeUnknown {
            operation: "backup.create",
            ..
        })
    ));
    assert_eq!(server.finish().len(), 4);

    let existing = leak(backup_response(
        "mysql",
        "mysql-1",
        "backup-1",
        "destination-1",
        "/initial/",
        "database-initial",
        Some(2),
    ));
    let before = parent_response("mysql", "mysql-1", &[existing.to_owned()]);
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", DESTINATIONS),
        ("200 OK", existing),
        ("200 OK", DESTINATIONS),
        ("200 OK", before),
        ("200 OK", "{}"),
        ("500 Internal Server Error", r#"{"message":"private"}"#),
    ]);
    assert!(matches!(
        client(&server)
            .backups()
            .update(update_input(target("mysql", "mysql-1")))
            .await,
        Err(Error::OutcomeUnknown {
            operation: "backup.update",
            ..
        })
    ));
    assert_eq!(server.finish().len(), 6);

    let existing = leak(backup_response(
        "mongo",
        "mongo-1",
        "backup-1",
        "destination-1",
        "/initial/",
        "database-initial",
        Some(2),
    ));
    let before = parent_response("mongo", "mongo-1", &[existing.to_owned()]);
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", existing),
        ("200 OK", DESTINATIONS),
        ("200 OK", before),
        ("200 OK", "{}"),
        ("500 Internal Server Error", r#"{"message":"private"}"#),
    ]);
    assert!(matches!(
        client(&server)
            .backups()
            .delete(BackupId::new("backup-1"), target("mongo", "mongo-1"))
            .await,
        Err(Error::OutcomeUnknown {
            operation: "backup.remove",
            ..
        })
    ));
    assert_eq!(server.finish().len(), 5);
}

#[tokio::test]
async fn missing_authoritative_relation_and_unsupported_direct_targets_fail_closed() {
    let server = TestServer::respond_in_sequence(vec![(
        "200 OK",
        r#"{"postgresId":"postgres-1","databasePassword":"backup-secret-canary-do-not-leak"}"#,
    )]);
    let error = client(&server)
        .backups()
        .by_target(target("postgres", "postgres-1"))
        .await
        .expect_err("missing backups relation fails closed");
    assert!(matches!(error, Error::Decode { .. }));
    assert!(!format!("{error:?}").contains(SECRET_CANARY));
    server.finish();

    let unsupported = serde_json::json!({
        "backupId":"backup-1",
        "schedule":"* * * * *",
        "enabled":false,
        "database":"dokploy",
        "prefix":"/",
        "destinationId":"destination-1",
        "keepLatestCount":null,
        "includeEncryptionKey":true,
        "backupType":"database",
        "databaseType":"web-server",
        "composeId":null,
        "postgresId":null,
        "mysqlId":null,
        "mariadbId":null,
        "mongoId":null,
        "libsqlId":null,
        "serviceName":null,
        "metadata":null
    })
    .to_string();
    let server = TestServer::respond_in_sequence(vec![("200 OK", leak(unsupported))]);
    let error = client(&server)
        .backups()
        .get(BackupId::new("backup-1"))
        .await
        .expect_err("web-server target is unsupported");
    assert!(matches!(error, Error::Decode { .. }));
    server.finish();
}

#[tokio::test]
async fn preflight_failures_are_definitive_and_do_not_mutate() {
    let server = TestServer::respond_in_sequence(vec![("200 OK", "[]")]);
    let error = client(&server)
        .backups()
        .create(create_input(target("postgres", "postgres-1")))
        .await
        .expect_err("destination preflight fails");
    assert!(
        matches!(error, Error::UnexpectedResponse { .. }),
        "{error:?}"
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /api/destination.all HTTP/1.1\r\n"));
}
