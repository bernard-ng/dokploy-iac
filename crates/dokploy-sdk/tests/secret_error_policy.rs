use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_api::{
    APPLICATION_CREATE, APPLICATION_DELETE, APPLICATION_DEPLOY, APPLICATION_ONE,
    APPLICATION_SAVE_ENVIRONMENT, APPLICATION_SEARCH, APPLICATION_UPDATE, BACKUP_CREATE,
    BACKUP_ONE, BACKUP_REMOVE, BACKUP_UPDATE, COMPOSE_CREATE, COMPOSE_DELETE, COMPOSE_ONE,
    COMPOSE_SEARCH, COMPOSE_UPDATE, DESTINATION_ALL, DOMAIN_BY_APPLICATION_ID, DOMAIN_CREATE,
    DOMAIN_DELETE, DOMAIN_ONE, DOMAIN_UPDATE, ENVIRONMENT_BY_PROJECT_ID, ENVIRONMENT_CREATE,
    ENVIRONMENT_ONE, ENVIRONMENT_REMOVE, ENVIRONMENT_UPDATE, Endpoint, EndpointMethod,
    LIBSQL_CREATE, LIBSQL_ONE, LIBSQL_REMOVE, LIBSQL_UPDATE, MARIADB_CHANGE_PASSWORD,
    MARIADB_CREATE, MARIADB_ONE, MARIADB_REMOVE, MARIADB_SEARCH, MARIADB_UPDATE,
    MONGO_CHANGE_PASSWORD, MONGO_CREATE, MONGO_ONE, MONGO_REMOVE, MONGO_SEARCH, MONGO_UPDATE,
    MOUNTS_CREATE, MOUNTS_LIST_BY_SERVICE_ID, MOUNTS_ONE, MOUNTS_REMOVE, MOUNTS_UPDATE,
    MYSQL_CHANGE_PASSWORD, MYSQL_CREATE, MYSQL_ONE, MYSQL_REMOVE, MYSQL_SEARCH, MYSQL_UPDATE,
    PORT_CREATE, PORT_DELETE, PORT_ONE, PORT_UPDATE, POSTGRES_CREATE, POSTGRES_ONE,
    POSTGRES_REMOVE, POSTGRES_SEARCH, POSTGRES_UPDATE, PROJECT_ALL, PROJECT_CREATE, PROJECT_ONE,
    PROJECT_REMOVE, PROJECT_UPDATE, REDIRECTS_CREATE, REDIRECTS_DELETE, REDIRECTS_ONE,
    REDIRECTS_UPDATE, REDIS_CREATE, REDIS_ONE, REDIS_REMOVE, REDIS_SEARCH, REDIS_UPDATE,
    REGISTRY_ALL, SCHEDULE_CREATE, SCHEDULE_DELETE, SCHEDULE_LIST, SCHEDULE_ONE, SCHEDULE_UPDATE,
    SECURITY_CREATE, SECURITY_DELETE, SECURITY_ONE, SECURITY_UPDATE, SERVER_ALL, TAG_ALL,
    TAG_ASSIGN_TO_PROJECT, TAG_CREATE, TAG_ONE, TAG_REMOVE, TAG_REMOVE_FROM_PROJECT, TAG_UPDATE,
};
use dokploy_sdk::{Dokploy, ImperativeRequest};

const SECRET_CANARY: &str = "secret-error-body-canary-do-not-retain";

const OWNED_ENDPOINTS: &[Endpoint] = &[
    PROJECT_ALL,
    PROJECT_CREATE,
    PROJECT_ONE,
    PROJECT_REMOVE,
    PROJECT_UPDATE,
    SERVER_ALL,
    REGISTRY_ALL,
    DESTINATION_ALL,
    BACKUP_CREATE,
    BACKUP_ONE,
    BACKUP_REMOVE,
    BACKUP_UPDATE,
    ENVIRONMENT_BY_PROJECT_ID,
    ENVIRONMENT_CREATE,
    ENVIRONMENT_ONE,
    ENVIRONMENT_REMOVE,
    ENVIRONMENT_UPDATE,
    APPLICATION_CREATE,
    APPLICATION_DELETE,
    APPLICATION_DEPLOY,
    APPLICATION_ONE,
    APPLICATION_SEARCH,
    APPLICATION_UPDATE,
    COMPOSE_CREATE,
    COMPOSE_DELETE,
    COMPOSE_ONE,
    COMPOSE_SEARCH,
    COMPOSE_UPDATE,
    MOUNTS_CREATE,
    MOUNTS_LIST_BY_SERVICE_ID,
    MOUNTS_ONE,
    MOUNTS_REMOVE,
    MOUNTS_UPDATE,
    PORT_CREATE,
    PORT_DELETE,
    PORT_ONE,
    PORT_UPDATE,
    REDIRECTS_CREATE,
    REDIRECTS_DELETE,
    REDIRECTS_ONE,
    REDIRECTS_UPDATE,
    SECURITY_CREATE,
    SECURITY_DELETE,
    SECURITY_ONE,
    SECURITY_UPDATE,
    SCHEDULE_CREATE,
    SCHEDULE_DELETE,
    SCHEDULE_LIST,
    SCHEDULE_ONE,
    SCHEDULE_UPDATE,
    DOMAIN_BY_APPLICATION_ID,
    DOMAIN_CREATE,
    DOMAIN_DELETE,
    DOMAIN_ONE,
    DOMAIN_UPDATE,
    POSTGRES_CREATE,
    POSTGRES_ONE,
    POSTGRES_REMOVE,
    POSTGRES_SEARCH,
    POSTGRES_UPDATE,
    LIBSQL_CREATE,
    LIBSQL_ONE,
    LIBSQL_REMOVE,
    LIBSQL_UPDATE,
    MYSQL_CREATE,
    MYSQL_CHANGE_PASSWORD,
    MYSQL_ONE,
    MYSQL_REMOVE,
    MYSQL_SEARCH,
    MYSQL_UPDATE,
    MARIADB_CREATE,
    MARIADB_CHANGE_PASSWORD,
    MARIADB_ONE,
    MARIADB_REMOVE,
    MARIADB_SEARCH,
    MARIADB_UPDATE,
    MONGO_CREATE,
    MONGO_CHANGE_PASSWORD,
    MONGO_ONE,
    MONGO_REMOVE,
    MONGO_SEARCH,
    MONGO_UPDATE,
    REDIS_CREATE,
    REDIS_ONE,
    REDIS_REMOVE,
    REDIS_SEARCH,
    REDIS_UPDATE,
    TAG_ALL,
    TAG_ASSIGN_TO_PROJECT,
    TAG_CREATE,
    TAG_ONE,
    TAG_REMOVE,
    TAG_REMOVE_FROM_PROJECT,
    TAG_UPDATE,
];

const PRESERVE_ERROR_BODY_ENDPOINTS: &[Endpoint] = &[
    PROJECT_CREATE,
    PROJECT_UPDATE,
    ENVIRONMENT_CREATE,
    ENVIRONMENT_UPDATE,
    APPLICATION_SEARCH,
    COMPOSE_SEARCH,
    DOMAIN_BY_APPLICATION_ID,
    DOMAIN_CREATE,
    DOMAIN_DELETE,
    DOMAIN_ONE,
    DOMAIN_UPDATE,
    PORT_CREATE,
    PORT_DELETE,
    PORT_ONE,
    PORT_UPDATE,
    REDIRECTS_CREATE,
    REDIRECTS_DELETE,
    REDIRECTS_ONE,
    REDIRECTS_UPDATE,
    POSTGRES_SEARCH,
    MYSQL_SEARCH,
    MYSQL_UPDATE,
    MARIADB_SEARCH,
    MARIADB_UPDATE,
    MONGO_SEARCH,
    MONGO_UPDATE,
    REDIS_SEARCH,
];

struct TestServer {
    url: String,
    request: Receiver<String>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn echoing_rejection() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, request) = mpsc::channel();
        let thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("test server accepts a request");
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).expect("request is readable");
                bytes.extend_from_slice(&buffer[..count]);
                if count == 0 || request_is_complete(&bytes) {
                    break;
                }
            }
            sender
                .send(String::from_utf8(bytes).expect("request is UTF-8"))
                .expect("request is delivered");

            let body = format!(
                r#"{{"code":"ECHOED_SECRET","message":"{SECRET_CANARY}","issues":[{{"message":"{SECRET_CANARY}"}}]}}"#,
            );
            write!(
                stream,
                "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body,
            )
            .expect("response is writable");
        });

        Self {
            url: format!("http://{address}"),
            request,
            thread,
        }
    }

    fn finish(self) -> String {
        let request = self.request.recv().expect("request is delivered");
        self.thread.join().expect("test server exits cleanly");
        request
    }
}

#[tokio::test]
async fn every_owned_endpoint_obeys_the_complete_policy_partition() {
    assert_owned_endpoint_partition();

    for endpoint in OWNED_ENDPOINTS {
        let server = TestServer::echoing_rejection();
        let error = client(&server)
            .imperative()
            .execute(request(*endpoint))
            .await
            .expect_err("the rejection is returned");
        let details = error.dokploy().expect("HTTP status remains structured");

        assert_eq!(details.status(), 400, "operation: {}", endpoint.operation());
        if PRESERVE_ERROR_BODY_ENDPOINTS.contains(endpoint) {
            assert_eq!(
                details.code(),
                "ECHOED_SECRET",
                "operation: {}",
                endpoint.operation()
            );
            assert_eq!(
                details.message(),
                SECRET_CANARY,
                "operation: {}",
                endpoint.operation()
            );
            assert_eq!(
                details.issues(),
                [SECRET_CANARY],
                "operation: {}",
                endpoint.operation()
            );
        } else {
            assert_sanitized(endpoint, &error);
        }

        let received = server.finish();
        assert!(received.starts_with(&format!(
            "{} /api/{} ",
            http_method(*endpoint),
            endpoint.operation(),
        )));
    }
}

#[tokio::test]
async fn an_unregistered_generated_endpoint_fails_closed() {
    assert!(!OWNED_ENDPOINTS.contains(&APPLICATION_SAVE_ENVIRONMENT));
    assert!(!PRESERVE_ERROR_BODY_ENDPOINTS.contains(&APPLICATION_SAVE_ENVIRONMENT));

    let server = TestServer::echoing_rejection();
    let error = client(&server)
        .imperative()
        .execute(request(APPLICATION_SAVE_ENVIRONMENT))
        .await
        .expect_err("the rejection is returned");

    assert_sanitized(&APPLICATION_SAVE_ENVIRONMENT, &error);
    server.finish();
}

fn assert_owned_endpoint_partition() {
    let operations = OWNED_ENDPOINTS
        .iter()
        .map(|endpoint| endpoint.operation())
        .collect::<HashSet<_>>();
    let preserved = PRESERVE_ERROR_BODY_ENDPOINTS
        .iter()
        .map(|endpoint| endpoint.operation())
        .collect::<HashSet<_>>();
    let sanitized = OWNED_ENDPOINTS
        .iter()
        .filter(|endpoint| !PRESERVE_ERROR_BODY_ENDPOINTS.contains(endpoint))
        .map(|endpoint| endpoint.operation())
        .collect::<HashSet<_>>();

    assert_eq!(operations.len(), OWNED_ENDPOINTS.len());
    assert!(preserved.is_disjoint(&sanitized));
    assert_eq!(
        preserved.union(&sanitized).copied().collect::<HashSet<_>>(),
        operations
    );
    assert!(OWNED_ENDPOINTS.contains(&APPLICATION_DEPLOY));
    assert!(!PRESERVE_ERROR_BODY_ENDPOINTS.contains(&APPLICATION_DEPLOY));
    assert!(
        PRESERVE_ERROR_BODY_ENDPOINTS
            .iter()
            .all(|endpoint| OWNED_ENDPOINTS.contains(endpoint))
    );
}

fn assert_sanitized(endpoint: &Endpoint, error: &dokploy_sdk::Error) {
    let details = error.dokploy().expect("HTTP status remains structured");

    assert_eq!(
        details.code(),
        "BAD_REQUEST",
        "operation: {}",
        endpoint.operation()
    );
    assert_eq!(
        details.message(),
        "Bad Request",
        "operation: {}",
        endpoint.operation()
    );
    assert!(
        details.issues().is_empty(),
        "operation: {}",
        endpoint.operation()
    );
    assert!(!error.to_string().contains(SECRET_CANARY));
    assert!(!format!("{error:?}").contains(SECRET_CANARY));
}

fn client(server: &TestServer) -> Dokploy {
    Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid")
}

fn request(endpoint: Endpoint) -> ImperativeRequest {
    match endpoint.method() {
        EndpointMethod::Get => ImperativeRequest::get(endpoint.operation()),
        EndpointMethod::Post => {
            ImperativeRequest::post(endpoint.operation()).body(serde_json::json!({}))
        }
        method => panic!("unsupported test endpoint method: {method:?}"),
    }
}

fn http_method(endpoint: Endpoint) -> &'static str {
    match endpoint.method() {
        EndpointMethod::Get => "GET",
        EndpointMethod::Post => "POST",
        method => panic!("unsupported test endpoint method: {method:?}"),
    }
}

fn request_is_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .map(str::to_owned)
        })
        .and_then(|length| length.parse::<usize>().ok())
        .unwrap_or_default();

    bytes.len() >= header_end + 4 + content_length
}
