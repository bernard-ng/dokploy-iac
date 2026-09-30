use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_api::{
    APPLICATION_DELETE, APPLICATION_ONE, APPLICATION_SEARCH, APPLICATION_UPDATE, COMPOSE_CREATE,
    COMPOSE_DELETE, COMPOSE_ONE, COMPOSE_SEARCH, COMPOSE_UPDATE, DESTINATION_ALL, DOMAIN_ONE,
    ENVIRONMENT_BY_PROJECT_ID, ENVIRONMENT_ONE, ENVIRONMENT_REMOVE, Endpoint, EndpointMethod,
    LIBSQL_CREATE, LIBSQL_ONE, LIBSQL_REMOVE, LIBSQL_UPDATE, MARIADB_CHANGE_PASSWORD,
    MARIADB_CREATE, MARIADB_ONE, MARIADB_REMOVE, MARIADB_UPDATE, MONGO_CHANGE_PASSWORD,
    MONGO_CREATE, MONGO_ONE, MONGO_REMOVE, MONGO_UPDATE, MOUNTS_CREATE, MOUNTS_LIST_BY_SERVICE_ID,
    MOUNTS_ONE, MOUNTS_REMOVE, MOUNTS_UPDATE, MYSQL_CHANGE_PASSWORD, MYSQL_CREATE, MYSQL_ONE,
    MYSQL_REMOVE, MYSQL_UPDATE, PORT_DELETE, PORT_ONE, POSTGRES_CREATE, POSTGRES_ONE,
    POSTGRES_REMOVE, POSTGRES_UPDATE, PROJECT_ALL, PROJECT_CREATE, PROJECT_ONE, PROJECT_REMOVE,
    REDIRECTS_ONE, REDIS_CREATE, REDIS_ONE, REDIS_REMOVE, REDIS_UPDATE, REGISTRY_ALL,
    SCHEDULE_CREATE, SCHEDULE_DELETE, SCHEDULE_LIST, SCHEDULE_ONE, SCHEDULE_UPDATE,
    SECURITY_CREATE, SECURITY_DELETE, SECURITY_ONE, SECURITY_UPDATE, SERVER_ALL,
};
use dokploy_sdk::{Dokploy, ImperativeRequest};

const SECRET_CANARY: &str = "secret-error-body-canary-do-not-retain";

const SENSITIVE_ENDPOINTS: &[Endpoint] = &[
    PROJECT_ALL,
    PROJECT_ONE,
    PROJECT_REMOVE,
    ENVIRONMENT_ONE,
    ENVIRONMENT_BY_PROJECT_ID,
    ENVIRONMENT_REMOVE,
    APPLICATION_ONE,
    APPLICATION_UPDATE,
    APPLICATION_DELETE,
    COMPOSE_ONE,
    COMPOSE_CREATE,
    COMPOSE_UPDATE,
    COMPOSE_DELETE,
    MOUNTS_ONE,
    MOUNTS_LIST_BY_SERVICE_ID,
    MOUNTS_CREATE,
    MOUNTS_UPDATE,
    MOUNTS_REMOVE,
    SECURITY_ONE,
    SECURITY_CREATE,
    SECURITY_UPDATE,
    SECURITY_DELETE,
    SCHEDULE_ONE,
    SCHEDULE_LIST,
    SCHEDULE_CREATE,
    SCHEDULE_UPDATE,
    SCHEDULE_DELETE,
    SERVER_ALL,
    REGISTRY_ALL,
    DESTINATION_ALL,
    POSTGRES_ONE,
    POSTGRES_CREATE,
    POSTGRES_UPDATE,
    POSTGRES_REMOVE,
    LIBSQL_ONE,
    LIBSQL_CREATE,
    LIBSQL_UPDATE,
    LIBSQL_REMOVE,
    MYSQL_ONE,
    MYSQL_CREATE,
    MYSQL_CHANGE_PASSWORD,
    MYSQL_REMOVE,
    MARIADB_ONE,
    MARIADB_CREATE,
    MARIADB_CHANGE_PASSWORD,
    MARIADB_REMOVE,
    MONGO_ONE,
    MONGO_CREATE,
    MONGO_CHANGE_PASSWORD,
    MONGO_REMOVE,
    REDIS_ONE,
    REDIS_CREATE,
    REDIS_UPDATE,
    REDIS_REMOVE,
];

const NON_SENSITIVE_CONTROLS: &[Endpoint] = &[
    PROJECT_CREATE,
    APPLICATION_SEARCH,
    COMPOSE_SEARCH,
    PORT_ONE,
    REDIRECTS_ONE,
    DOMAIN_ONE,
    PORT_DELETE,
    MYSQL_UPDATE,
    MARIADB_UPDATE,
    MONGO_UPDATE,
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
async fn every_sensitive_endpoint_discards_echoed_error_bodies() {
    for endpoint in SENSITIVE_ENDPOINTS {
        let server = TestServer::echoing_rejection();
        let error = client(&server)
            .imperative()
            .execute(request(*endpoint))
            .await
            .expect_err("the rejection is returned");
        let details = error.dokploy().expect("HTTP status remains structured");

        assert_eq!(details.status(), 400, "operation: {}", endpoint.operation());
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

        let received = server.finish();
        assert!(received.starts_with(&format!(
            "{} /api/{} ",
            http_method(*endpoint),
            endpoint.operation(),
        )));
    }
}

#[tokio::test]
async fn explicit_non_sensitive_controls_preserve_structured_error_details() {
    for endpoint in NON_SENSITIVE_CONTROLS {
        let server = TestServer::echoing_rejection();
        let error = client(&server)
            .imperative()
            .execute(request(*endpoint))
            .await
            .expect_err("the rejection is returned");
        let details = error.dokploy().expect("remote details are preserved");

        assert_eq!(details.status(), 400, "operation: {}", endpoint.operation());
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
        assert!(error.to_string().contains(SECRET_CANARY));

        server.finish();
    }
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
