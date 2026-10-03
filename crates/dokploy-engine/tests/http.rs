//! The same engine, reached over real HTTP instead of in memory.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::{self, JoinHandle};

use common::{compile, engine};
use dokploy_core::{ChangeKind, PlanDiagnosticCode};
use dokploy_sdk::Dokploy;

const DOCUMENT: &str = "\
version: 2
settings:
  registries:
    ghcr:
      type: cloud
      url: ghcr.io
      username: ci
      password:
        env: GHCR_TOKEN
";

fn serve(responses: Vec<(&'static str, &'static str)>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().expect("accepts");
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).expect("reads");
                bytes.extend_from_slice(&buffer[..count]);
                if count == 0 || bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            requests.push(String::from_utf8_lossy(&bytes).into_owned());
            write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        requests
    });
    (url, handle)
}

fn client(url: &str) -> Dokploy {
    Dokploy::builder().url(url).api_key("key").build().unwrap()
}

#[tokio::test]
async fn a_registry_is_planned_over_http_with_reads_only() {
    let (url, server) = serve(vec![("200 OK", "[]")]);
    let engine = engine(client(&url));
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", "t")]);

    let plan = engine.plan(&compiled, None).await.unwrap();

    assert!(plan.applyable(), "{plan:?}");
    assert_eq!(plan.changes()[0].kind(), ChangeKind::Create);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].starts_with("GET /api/registry.all HTTP/1.1"),
        "{}",
        requests[0]
    );
    assert!(
        requests.iter().all(|r| r.starts_with("GET ")),
        "planning never mutates"
    );
}

#[tokio::test]
async fn rejected_credentials_block_the_plan_and_say_so() {
    let body = r#"{"message":"Unauthorized","code":"UNAUTHORIZED"}"#;
    let (url, server) = serve(vec![("401 Unauthorized", body)]);
    let engine = engine(client(&url));
    let compiled = compile(&engine, DOCUMENT, &[("GHCR_TOKEN", "t")]);

    let plan = engine.plan(&compiled, None).await.unwrap();

    assert!(!plan.applyable(), "{plan:?}");
    assert!(
        plan.diagnostics()
            .iter()
            .any(|d| d.code() == PlanDiagnosticCode::RemoteUnavailable)
    );
    server.join().unwrap();
}
