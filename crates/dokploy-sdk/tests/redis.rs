use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{Dokploy, EnvironmentId, Error, ProjectDetails, RedisDetails, RedisId};

const REDIS_FIXTURE: &str = include_str!("../../../fixtures/api/live/v0.30.6/redis-one.owner.json");

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

impl TestServer {
    fn respond_with_json(body: &'static str) -> Self {
        Self::respond_in_sequence(vec![body])
    }

    fn respond_in_sequence(bodies: Vec<&'static str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (request_sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut received_requests = Vec::new();

            for body in bodies {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 1024];

                loop {
                    let count = stream.read(&mut buffer).expect("request is readable");
                    bytes.extend_from_slice(&buffer[..count]);
                    if count == 0 || bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }

                received_requests.push(String::from_utf8(bytes).expect("request is UTF-8"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response is writable");
            }

            request_sender
                .send(received_requests)
                .expect("test receives requests");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().expect("test receives requests");
        self.thread.join().expect("test server exits cleanly");
        requests
    }
}

#[test]
fn redis_details_expose_only_safe_reconciliation_fields() {
    let redis: RedisDetails =
        serde_json::from_str(REDIS_FIXTURE).expect("fixture must deserialize");

    assert_eq!(redis.redis_id.as_str(), "redis-1");
    assert_eq!(redis.environment_id.as_str(), "environment-1");
    assert_eq!(redis.name, "Redis Contract Test");
    assert_eq!(redis.app_name, "redis-contract-test");
    assert_eq!(redis.docker_image, "redis:8");

    let debug = format!("{redis:?}");
    assert!(!debug.contains("<redacted>"));
    assert!(!debug.contains("database_password"));
    assert!(!debug.contains("env:"));
}

#[test]
fn project_details_decode_the_sanitized_redis_summary_without_secret_fields() {
    let project: ProjectDetails = serde_json::from_str(include_str!(
        "../../../fixtures/api/live/v0.30.6/project-one.redis-populated.owner.json"
    ))
    .expect("Redis project fixture must deserialize");
    let redis = &project.environments[0].redis[0];

    assert_eq!(redis.redis_id.as_str(), "redis-1");
    assert_eq!(redis.name.as_deref(), Some("Redis Contract Test"));
    assert!(!format!("{project:?}").contains("databasePassword"));
}

#[tokio::test]
async fn redis_get_uses_a_strong_id_and_decodes_safe_details() {
    let server = TestServer::respond_with_json(REDIS_FIXTURE);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let redis = client
        .redis()
        .get(RedisId::new("redis-1"))
        .await
        .expect("Redis database is readable");

    let requests = server.finish();
    assert!(requests[0].starts_with("GET /api/redis.one?redisId=redis-1 HTTP/1.1\r\n"));
    assert_eq!(redis.redis_id.as_str(), "redis-1");
}

#[tokio::test]
async fn redis_by_environment_collects_every_bounded_page() {
    let first_page_items = (0..100)
        .map(|index| {
            format!(
                r#"{{"redisId":"redis-{index}","environmentId":"environment-1","name":"cache-{index}"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let first_page = format!(r#"{{"items":[{first_page_items}],"total":101}}"#);
    let first_page: &'static str = Box::leak(first_page.into_boxed_str());
    let server = TestServer::respond_in_sequence(vec![
        first_page,
        r#"{"items":[{"redisId":"redis-100","environmentId":"environment-1","name":"cache-100"}],"total":101}"#,
    ]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let collection = client
        .redis()
        .by_environment(EnvironmentId::new("environment-1"))
        .await
        .expect("Redis collection is readable");

    assert_eq!(collection.redis().len(), 101);
    let requests = server.finish();
    assert!(requests[0].starts_with("GET /api/redis.search?"));
    assert!(requests[0].contains("environmentId=environment-1"));
    assert!(requests[0].contains("limit=100"));
    assert!(requests[0].contains("offset=0"));
    assert!(requests[1].contains("offset=100"));
}

#[tokio::test]
async fn redis_by_environment_rejects_malformed_pagination() {
    let hundred_items = (0..100)
        .map(|index| {
            format!(
                r#"{{"redisId":"redis-{index}","environmentId":"environment-1","name":"cache-{index}"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let hundred_and_one_items = (0..101)
        .map(|index| {
            format!(
                r#"{{"redisId":"redis-{index}","environmentId":"environment-1","name":"cache-{index}"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let first_unstable =
        Box::leak(format!(r#"{{"items":[{hundred_items}],"total":101}}"#).into_boxed_str());
    let oversized =
        Box::leak(format!(r#"{{"items":[{hundred_and_one_items}],"total":101}}"#).into_boxed_str());
    let cases: Vec<Vec<&'static str>> = vec![
        vec![r#"{"items":[],"total":1}"#],
        vec![
            r#"{"items":[{"redisId":"redis-1","environmentId":"environment-1","name":"one"},{"redisId":"redis-2","environmentId":"environment-1","name":"two"}],"total":1}"#,
        ],
        vec![r#"{"items":[],"total":10001}"#],
        vec![first_unstable, r#"{"items":[],"total":100}"#],
        vec![oversized],
    ];

    for responses in cases {
        let server = TestServer::respond_in_sequence(responses);
        let client = Dokploy::builder()
            .url(&server.url)
            .api_key("test-api-key")
            .build()
            .expect("client configuration is valid");

        let error = client
            .redis()
            .by_environment(EnvironmentId::new("environment-1"))
            .await
            .expect_err("malformed pagination must fail closed");

        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "redis.search"
            }
        ));
        server.finish();
    }
}

#[tokio::test]
async fn redis_search_rejects_an_empty_environment_before_transport() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .expect("client configuration is valid");

    let error = client
        .redis()
        .by_environment(EnvironmentId::new(""))
        .await
        .expect_err("an empty environment ID is invalid");

    assert!(matches!(
        error,
        Error::InvalidRequest {
            operation: "redis.search",
            ..
        }
    ));
}
