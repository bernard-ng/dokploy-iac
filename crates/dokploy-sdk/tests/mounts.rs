use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    ApplicationId, ComposeId, CreateMount, Dokploy, Error, LibSqlId, MariaDbId, MongoId,
    MountDetails, MountId, MountType, MySqlId, PostgresId, RedisId, ResponseField, ServiceTarget,
    UpdateMount,
};
use zeroize::Zeroizing;

const MOUNT_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/mount-one.created.owner.json");
const VOLUME_RESPONSE: &str = r#"{
    "mountId":"mount-1",
    "type":"volume",
    "hostPath":null,
    "volumeName":"volume-1",
    "filePath":null,
    "content":null,
    "serviceType":"application",
    "mountPath":"/data",
    "applicationId":"application-1",
    "composeId":null,
    "libsqlId":null,
    "mariadbId":null,
    "mongoId":null,
    "mysqlId":null,
    "postgresId":null,
    "redisId":null
}"#;

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_with_json(body: &'static str) -> Self {
        Self::respond_in_sequence(vec![("200 OK", body)])
    }

    fn respond_in_sequence(responses: Vec<(&'static str, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received_requests = Vec::new();

            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                let bytes = read_request(&mut stream);
                received_requests.push(String::from_utf8(bytes).expect("request is UTF-8"));
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response is writable");
            }

            sender
                .send(received_requests)
                .expect("test receives the requests");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn close_after_request() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("test server accepts a request");
            let bytes = read_request(&mut stream);
            sender
                .send(vec![String::from_utf8(bytes).expect("request is UTF-8")])
                .expect("test receives the request");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn finish(self) -> String {
        let mut requests = self.finish_all();

        assert_eq!(requests.len(), 1, "test server expected one request");
        requests.remove(0)
    }

    fn finish_all(self) -> Vec<String> {
        let requests = self.requests.recv().expect("test receives the requests");
        self.thread.join().expect("test server exits cleanly");

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
    let content_length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default();

    bytes.len() >= header_end + 4 + content_length
}

fn client(server: &TestServer) -> Dokploy {
    Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid")
}

fn application_target() -> ServiceTarget {
    ServiceTarget::Application(ApplicationId::new("application-1"))
}

fn assert_mutation_request(request: &str, operation: &str, expected: serde_json::Value) {
    assert!(request.starts_with(&format!("POST /api/{operation} HTTP/1.1\r\n")));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-api-key: test-api-key")
    );
    let body = request
        .split_once("\r\n\r\n")
        .expect("request contains a body")
        .1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).expect("request body is JSON"),
        expected
    );
}

#[test]
fn mount_fixture_exposes_only_safe_typed_fields() {
    let mount: MountDetails = serde_json::from_str(MOUNT_FIXTURE).expect("fixture deserializes");

    assert_eq!(mount.mount_id.as_str(), "mount-1");
    assert_eq!(mount.mount_type, MountType::Volume);
    assert_eq!(mount.mount_path, "/data");
    assert_eq!(mount.target, application_target());
    assert_eq!(
        mount.volume_name,
        ResponseField::Value("volume-1".to_owned())
    );

    let debug = format!("{mount:?}");
    assert!(!debug.contains("content"));
    assert!(!debug.contains("file-content-canary"));
    assert!(!MOUNT_FIXTURE.contains("file-content-canary"));

    let contradictory =
        MOUNT_FIXTURE.replace("\"composeId\": null", "\"composeId\": \"compose-1\"");
    serde_json::from_str::<MountDetails>(&contradictory)
        .expect_err("multiple target identities must fail closed");

    let minimal: MountDetails = serde_json::from_str(
        r#"{
            "mountId":"mount-2",
            "type":"volume",
            "mountPath":"/minimal",
            "serviceType":"application",
            "applicationId":"application-1",
            "content":"minimal-file-content-canary",
            "futureRuntimeField":{"nested":true}
        }"#,
    )
    .expect("minimal Mount responses tolerate omitted and unknown fields");
    assert_eq!(minimal.volume_name, ResponseField::NotReturned);
    assert!(!format!("{minimal:?}").contains("minimal-file-content-canary"));
}

#[test]
fn every_service_target_uses_its_exact_wire_kind_and_identity() {
    let cases = [
        (application_target(), "application", "application-1"),
        (
            ServiceTarget::Compose(ComposeId::new("compose-1")),
            "compose",
            "compose-1",
        ),
        (
            ServiceTarget::LibSql(LibSqlId::new("libsql-1")),
            "libsql",
            "libsql-1",
        ),
        (
            ServiceTarget::MariaDb(MariaDbId::new("mariadb-1")),
            "mariadb",
            "mariadb-1",
        ),
        (
            ServiceTarget::Mongo(MongoId::new("mongo-1")),
            "mongo",
            "mongo-1",
        ),
        (
            ServiceTarget::MySql(MySqlId::new("mysql-1")),
            "mysql",
            "mysql-1",
        ),
        (
            ServiceTarget::Postgres(PostgresId::new("postgres-1")),
            "postgres",
            "postgres-1",
        ),
        (
            ServiceTarget::Redis(RedisId::new("redis-1")),
            "redis",
            "redis-1",
        ),
    ];

    for (target, service_type, service_id) in cases {
        let value = serde_json::to_value(CreateMount::volume(target, "volume-1", "/data"))
            .expect("Mount input serializes");

        assert_eq!(value["serviceType"], service_type);
        assert_eq!(value["serviceId"], service_id);
    }
}

#[tokio::test]
async fn mount_get_uses_a_strong_id_and_safe_tolerant_model() {
    let server = TestServer::respond_with_json(VOLUME_RESPONSE);
    let mount = client(&server)
        .mounts()
        .get(MountId::new("mount-1"))
        .await
        .expect("Mount is readable");

    let request = server.finish();
    assert!(request.starts_with("GET /api/mounts.one?mountId=mount-1 HTTP/1.1\r\n"));
    assert_eq!(mount.target, application_target());
}

#[tokio::test]
async fn mount_list_is_bounded_and_authoritative_for_one_typed_target() {
    let response: &'static str = Box::leak(format!("[{VOLUME_RESPONSE}]").into_boxed_str());
    let server = TestServer::respond_with_json(response);
    let collection = client(&server)
        .mounts()
        .by_target(application_target())
        .await
        .expect("target Mount collection is readable");

    assert_eq!(collection.mounts().len(), 1);
    let request = server.finish();
    assert!(request.starts_with("GET /api/mounts.listByServiceId?"));
    assert!(request.contains("serviceType=application"));
    assert!(request.contains("serviceId=application-1"));

    let duplicate: &'static str =
        Box::leak(format!("[{VOLUME_RESPONSE},{VOLUME_RESPONSE}]").into_boxed_str());
    let wrong_target = VOLUME_RESPONSE.replace("application-1", "application-2");
    let wrong_target: &'static str = Box::leak(format!("[{wrong_target}]").into_boxed_str());
    let too_many = (0..10_001)
        .map(|index| VOLUME_RESPONSE.replace("mount-1", &format!("mount-{index}")))
        .collect::<Vec<_>>()
        .join(",");
    let too_many: &'static str = Box::leak(format!("[{too_many}]").into_boxed_str());

    for body in [duplicate, wrong_target, too_many] {
        let server = TestServer::respond_with_json(body);
        let error = client(&server)
            .mounts()
            .by_target(application_target())
            .await
            .expect_err("contradictory target evidence must fail closed");

        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "mounts.listByServiceId"
            }
        ));
        server.finish();
    }
}

#[tokio::test]
async fn mount_create_uses_the_exact_typed_volume_contract() {
    let server = TestServer::respond_with_json(VOLUME_RESPONSE);
    let created = client(&server)
        .mounts()
        .create(CreateMount::volume(
            application_target(),
            "volume-1",
            "/data",
        ))
        .await
        .expect("Mount creation succeeds");

    assert_eq!(created.mount_id().as_str(), "mount-1");
    assert_mutation_request(
        &server.finish(),
        "mounts.create",
        serde_json::json!({
            "type": "volume",
            "volumeName": "volume-1",
            "mountPath": "/data",
            "serviceType": "application",
            "serviceId": "application-1"
        }),
    );

    let mismatch = TestServer::respond_with_json(Box::leak(
        VOLUME_RESPONSE
            .replace("application-1", "application-2")
            .into_boxed_str(),
    ));
    let error = client(&mismatch)
        .mounts()
        .create(CreateMount::volume(
            application_target(),
            "volume-1",
            "/data",
        ))
        .await
        .expect_err("a conflicting returned target must fail closed");
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "mounts.create"
        }
    ));
    mismatch.finish();
}

#[tokio::test]
async fn file_mount_content_is_redacted_but_sent_exactly_once() {
    let file_response = VOLUME_RESPONSE
        .replace("\"type\":\"volume\"", "\"type\":\"file\"")
        .replace("\"volumeName\":\"volume-1\"", "\"volumeName\":null")
        .replace("\"filePath\":null", "\"filePath\":\"settings.env\"")
        .replace("\"content\":null", "\"content\":\"file-content-canary\"")
        .replace(
            "\"mountPath\":\"/data\"",
            "\"mountPath\":\"/run/settings.env\"",
        );
    let file_response: &'static str = Box::leak(file_response.into_boxed_str());
    let create_server = TestServer::respond_with_json(file_response);
    let create = CreateMount::file(
        application_target(),
        "settings.env",
        "/run/settings.env",
        Zeroizing::new("file-content-canary".to_owned()),
    );
    assert!(!format!("{create:?}").contains("file-content-canary"));
    client(&create_server)
        .mounts()
        .create(create)
        .await
        .expect("file Mount creation succeeds");
    assert_mutation_request(
        &create_server.finish(),
        "mounts.create",
        serde_json::json!({
            "type": "file",
            "filePath": "settings.env",
            "content": "file-content-canary",
            "mountPath": "/run/settings.env",
            "serviceType": "application",
            "serviceId": "application-1"
        }),
    );

    let update_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    let update = UpdateMount::new(MountId::new("mount-1")).with_file(
        "updated.env",
        Zeroizing::new("updated-file-content-canary".to_owned()),
    );
    assert!(!format!("{update:?}").contains("updated-file-content-canary"));
    client(&update_server)
        .mounts()
        .update(update)
        .await
        .expect("file Mount update succeeds");
    assert_mutation_request(
        &update_server.finish(),
        "mounts.update",
        serde_json::json!({
            "mountId": "mount-1",
            "type": "file",
            "filePath": "updated.env",
            "content": "updated-file-content-canary"
        }),
    );
}

#[tokio::test]
async fn mount_update_and_remove_use_narrow_explicit_requests() {
    let update_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    client(&update_server)
        .mounts()
        .update(UpdateMount::new(MountId::new("mount-1")).with_mount_path("/updated"))
        .await
        .expect("Mount update succeeds");
    assert_mutation_request(
        &update_server.finish(),
        "mounts.update",
        serde_json::json!({"mountId": "mount-1", "mountPath": "/updated"}),
    );

    let remove_server = TestServer::respond_with_json(r#"{"ok":true}"#);
    client(&remove_server)
        .mounts()
        .delete(MountId::new("mount-1"))
        .await
        .expect("Mount removal succeeds");
    assert_mutation_request(
        &remove_server.finish(),
        "mounts.remove",
        serde_json::json!({"mountId": "mount-1"}),
    );
}

#[tokio::test]
async fn mount_rejects_invalid_inputs_before_transport_without_exposing_content() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let errors = [
        client
            .mounts()
            .get(MountId::new(""))
            .await
            .expect_err("an empty Mount ID is invalid"),
        client
            .mounts()
            .by_target(ServiceTarget::Application(ApplicationId::new("")))
            .await
            .expect_err("an empty target ID is invalid"),
        client
            .mounts()
            .create(CreateMount::file(
                application_target(),
                "",
                "/run/settings.env",
                Zeroizing::new("invalid-file-content-canary".to_owned()),
            ))
            .await
            .expect_err("an empty file path is invalid"),
        client
            .mounts()
            .update(UpdateMount::new(MountId::new("mount-1")))
            .await
            .expect_err("an empty update is invalid"),
        client
            .mounts()
            .delete(MountId::new(""))
            .await
            .expect_err("an empty Mount ID is invalid"),
    ];

    assert!(errors.iter().all(|error| matches!(
        error,
        Error::InvalidRequest { operation, .. } if operation.starts_with("mounts.")
    )));
    assert!(
        errors
            .iter()
            .all(|error| !format!("{error:?}").contains("invalid-file-content-canary"))
    );
}

#[tokio::test]
async fn mount_mutations_are_single_attempt_and_report_unknown_outcomes() {
    let create_server = TestServer::close_after_request();
    let create_error = client(&create_server)
        .mounts()
        .create(CreateMount::volume(
            application_target(),
            "volume-1",
            "/data",
        ))
        .await
        .expect_err("missing create response has an unknown outcome");
    assert!(matches!(
        create_error,
        Error::OutcomeUnknown {
            operation: "mounts.create",
            ..
        }
    ));
    assert_eq!(create_server.finish_all().len(), 1);

    let update_server = TestServer::close_after_request();
    let update_error = client(&update_server)
        .mounts()
        .update(UpdateMount::new(MountId::new("mount-1")).with_mount_path("/updated"))
        .await
        .expect_err("missing update response has an unknown outcome");
    assert!(matches!(
        update_error,
        Error::OutcomeUnknown {
            operation: "mounts.update",
            ..
        }
    ));
    assert_eq!(update_server.finish_all().len(), 1);

    let remove_server = TestServer::close_after_request();
    let remove_error = client(&remove_server)
        .mounts()
        .delete(MountId::new("mount-1"))
        .await
        .expect_err("missing remove response has an unknown outcome");
    assert!(matches!(
        remove_error,
        Error::OutcomeUnknown {
            operation: "mounts.remove",
            ..
        }
    ));
    assert_eq!(remove_server.finish_all().len(), 1);
}
