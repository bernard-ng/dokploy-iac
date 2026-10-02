use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use dokploy_sdk::{
    ApplicationId, ComposeId, CreateSchedule, Dokploy, Error, ScheduleDetails, ScheduleId,
    ScheduleTarget, ShellType, UpdateSchedule,
};
use zeroize::Zeroizing;

const COMMAND_CANARY: &str = "schedule-command-canary-do-not-leak";
const SCRIPT_CANARY: &str = "schedule-script-canary-do-not-leak";
const RESPONSE_COMMAND_CANARY: &str = "response-command-canary-do-not-leak";
const RESPONSE_SCRIPT_CANARY: &str = "response-script-canary-do-not-leak";
const SCHEDULE_FIXTURE: &str =
    include_str!("../../../fixtures/api/live/v0.30.6/schedule-one.application-created.owner.json");
const APPLICATION_RESPONSE: &str = r#"{
    "scheduleId":"schedule-1",
    "name":"application-job",
    "description":"initial",
    "cronExpression":"0 0 * * *",
    "appName":"runtime-app-name",
    "serviceName":null,
    "shellType":"bash",
    "scheduleType":"application",
    "command":"response-command-canary-do-not-leak",
    "script":"response-script-canary-do-not-leak",
    "applicationId":"application-1",
    "composeId":null,
    "serverId":null,
    "organizationId":null,
    "enabled":false,
    "timezone":"UTC",
    "createdAt":"2026-09-30T00:00:00.000Z",
    "application":{"env":"nested-secret-canary"},
    "deployments":[]
}"#;
const COMPOSE_RESPONSE: &str = r#"{
    "scheduleId":"schedule-2",
    "name":"compose-job",
    "description":null,
    "cronExpression":"5 0 * * *",
    "appName":"runtime-compose-name",
    "serviceName":"worker",
    "shellType":"sh",
    "scheduleType":"compose",
    "command":"response-command-canary-do-not-leak",
    "script":null,
    "applicationId":null,
    "composeId":"compose-1",
    "serverId":null,
    "organizationId":null,
    "enabled":false,
    "timezone":"UTC",
    "createdAt":"2026-09-30T00:00:00.000Z",
    "compose":{"env":"nested-secret-canary"},
    "deployments":[]
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

    fn close_after_requests(responses: Vec<(&'static str, &'static str)>) -> Self {
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
            let (mut stream, _) = listener.accept().expect("test server accepts mutation");
            received.push(String::from_utf8(read_request(&mut stream)).expect("UTF-8 request"));
            sender.send(received).expect("test receives requests");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn finish(self) -> String {
        let mut requests = self.finish_all();
        assert_eq!(requests.len(), 1);
        requests.remove(0)
    }

    fn finish_all(self) -> Vec<String> {
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

fn application_target() -> ScheduleTarget {
    ScheduleTarget::Application(ApplicationId::new("application-1"))
}

fn compose_target() -> ScheduleTarget {
    ScheduleTarget::Compose {
        compose_id: ComposeId::new("compose-1"),
        service_name: "worker".to_owned(),
    }
}

fn privileged_response(schedule_type: &str) -> &'static str {
    Box::leak(
        APPLICATION_RESPONSE
            .replace(
                "\"scheduleType\":\"application\"",
                &format!("\"scheduleType\":\"{schedule_type}\""),
            )
            .replace(
                "\"applicationId\":\"application-1\"",
                "\"applicationId\":null",
            )
            .replace("\"serverId\":null", "\"serverId\":\"server-1\"")
            .into_boxed_str(),
    )
}

fn create_input(target: ScheduleTarget, name: &str) -> CreateSchedule {
    CreateSchedule::new(
        target,
        name,
        Some("initial".to_owned()),
        "0 0 * * *",
        ShellType::Bash,
        Zeroizing::new(COMMAND_CANARY.to_owned()),
        Some(Zeroizing::new(SCRIPT_CANARY.to_owned())),
        false,
        Some("UTC".to_owned()),
    )
}

fn list_with(entries: &str) -> &'static str {
    Box::leak(format!("[{entries}]").into_boxed_str())
}

fn proof_response(response: &str) -> &'static str {
    proof_response_with_secrets(response, COMMAND_CANARY, Some(SCRIPT_CANARY))
}

fn proof_response_with_secrets(
    response: &str,
    command: &str,
    script: Option<&str>,
) -> &'static str {
    let script = script.map_or_else(
        || "null".to_owned(),
        |value| serde_json::to_string(value).unwrap(),
    );
    Box::leak(
        response
            .replace(RESPONSE_COMMAND_CANARY, command)
            .replace(&format!("\"{RESPONSE_SCRIPT_CANARY}\""), &script)
            .into_boxed_str(),
    )
}

fn assert_request(request: &str, operation: &str, expected: serde_json::Value) {
    assert!(request.starts_with(&format!("POST /api/{operation} HTTP/1.1\r\n")));
    let body = request.split_once("\r\n\r\n").unwrap().1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).unwrap(),
        expected
    );
}

fn assert_no_canary(value: impl std::fmt::Debug) {
    let rendered = format!("{value:?}");
    for canary in [
        COMMAND_CANARY,
        SCRIPT_CANARY,
        RESPONSE_COMMAND_CANARY,
        RESPONSE_SCRIPT_CANARY,
        "nested-secret-canary",
    ] {
        assert!(!rendered.contains(canary), "canary leaked: {rendered}");
    }
}

#[test]
fn safe_schedule_models_consume_commands_and_reject_privileged_targets() {
    let application: ScheduleDetails = serde_json::from_str(APPLICATION_RESPONSE).unwrap();
    assert_eq!(application.target, application_target());
    assert!(application.command_present);
    assert!(application.script_present);
    assert_no_canary(&application);

    let compose: ScheduleDetails = serde_json::from_str(COMPOSE_RESPONSE).unwrap();
    assert_eq!(compose.target, compose_target());
    assert!(compose.command_present);
    assert!(!compose.script_present);
    assert_no_canary(&compose);

    let fixture: ScheduleDetails = serde_json::from_str(SCHEDULE_FIXTURE).unwrap();
    assert!(fixture.command_present);
    assert!(fixture.script_present);
    assert!(SCHEDULE_FIXTURE.contains("<redacted>"));

    for schedule_type in ["server", "dokploy-server"] {
        let error = serde_json::from_str::<ScheduleDetails>(privileged_response(schedule_type))
            .unwrap_err();
        assert_no_canary(&error);
    }
}

#[test]
fn schedule_inputs_serialize_exact_target_shapes_and_redact_debug() {
    let application = create_input(application_target(), "application-job");
    assert_no_canary(&application);
    assert_eq!(
        serde_json::to_value(&application).unwrap(),
        serde_json::json!({
            "name":"application-job","description":"initial","cronExpression":"0 0 * * *",
            "shellType":"bash","command":COMMAND_CANARY,"script":SCRIPT_CANARY,"enabled":false,
            "timezone":"UTC","scheduleType":"application","applicationId":"application-1"
        })
    );

    let compose = create_input(compose_target(), "compose-job");
    assert_no_canary(&compose);
    assert_eq!(
        serde_json::to_value(&compose).unwrap(),
        serde_json::json!({
            "name":"compose-job","description":"initial","cronExpression":"0 0 * * *",
            "shellType":"bash","command":COMMAND_CANARY,"script":SCRIPT_CANARY,"enabled":false,
            "timezone":"UTC","scheduleType":"compose","composeId":"compose-1","serviceName":"worker"
        })
    );
}

#[tokio::test]
async fn schedule_get_requires_direct_and_authoritative_list_agreement() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", APPLICATION_RESPONSE),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
    ]);
    let details = client(&server)
        .schedules()
        .get(ScheduleId::new("schedule-1"))
        .await
        .unwrap();
    assert_no_canary(&details);
    let requests = server.finish_all();
    assert!(requests[0].starts_with("GET /api/schedule.one?scheduleId=schedule-1 HTTP/1.1\r\n"));
    assert!(requests[1].starts_with(
        "GET /api/schedule.list?id=application-1&scheduleType=application HTTP/1.1\r\n"
    ));

    let conflict = APPLICATION_RESPONSE.replace("\"enabled\":false", "\"enabled\":true");
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", APPLICATION_RESPONSE),
        ("200 OK", list_with(Box::leak(conflict.into_boxed_str()))),
    ]);
    let error = client(&server)
        .schedules()
        .get(ScheduleId::new("schedule-1"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "schedule.one"
        }
    ));
    assert_no_canary(&error);
    server.finish_all();
}

#[tokio::test]
async fn schedule_reads_never_retain_echoed_commands_or_scripts() {
    let echoed = r#"{
        "code":"ECHOED_COMMAND",
        "message":"schedule-command-canary-do-not-leak",
        "issues":[{"message":"schedule-script-canary-do-not-leak"}]
    }"#;
    for (operation, action) in [("schedule.one", "one"), ("schedule.list", "list")] {
        let server = TestServer::respond_in_sequence(vec![("400 Bad Request", echoed)]);
        let client = client(&server);
        let error = if action == "one" {
            client
                .schedules()
                .get(ScheduleId::new("schedule-1"))
                .await
                .unwrap_err()
        } else {
            client
                .schedules()
                .by_target(application_target())
                .await
                .unwrap_err()
        };
        let dokploy = error.dokploy().expect("HTTP status remains structured");
        assert_eq!(dokploy.status(), 400, "{operation}");
        assert_eq!(dokploy.code(), "BAD_REQUEST", "{operation}");
        assert_eq!(dokploy.message(), "Bad Request", "{operation}");
        assert!(dokploy.issues().is_empty(), "{operation}");
        assert_no_canary(&error);
        server.finish();
    }
}

#[tokio::test]
async fn schedule_list_is_bounded_target_consistent_and_unique() {
    for (target, response, expected_query) in [
        (
            application_target(),
            APPLICATION_RESPONSE,
            "id=application-1&scheduleType=application",
        ),
        (
            compose_target(),
            COMPOSE_RESPONSE,
            "id=compose-1&scheduleType=compose",
        ),
    ] {
        let server = TestServer::respond_with_json(list_with(response));
        let collection = client(&server)
            .schedules()
            .by_target(target.clone())
            .await
            .unwrap();
        assert_eq!(collection.target(), &target);
        assert_eq!(collection.schedules().len(), 1);
        assert_no_canary(&collection);
        assert!(server.finish().contains(expected_query));
    }

    let duplicate_name = APPLICATION_RESPONSE.replace("schedule-1", "schedule-2");
    let duplicate_id = APPLICATION_RESPONSE.replace("application-job", "other-job");
    let wrong_target = APPLICATION_RESPONSE.replace("application-1", "application-2");
    let too_many = (0..10_001)
        .map(|index| {
            APPLICATION_RESPONSE
                .replace("schedule-1", &format!("schedule-{index}"))
                .replace("application-job", &format!("job-{index}"))
        })
        .collect::<Vec<_>>()
        .join(",");
    for response in [
        list_with(&format!("{APPLICATION_RESPONSE},{duplicate_name}")),
        list_with(&format!("{APPLICATION_RESPONSE},{duplicate_id}")),
        list_with(&wrong_target),
        list_with(&too_many),
    ] {
        let server = TestServer::respond_with_json(response);
        let error = client(&server)
            .schedules()
            .by_target(application_target())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "schedule.list"
            }
        ));
        assert_no_canary(&error);
        server.finish();
    }
}

#[tokio::test]
async fn schedule_create_checks_collision_returned_identity_and_list() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        ("200 OK", proof_response(APPLICATION_RESPONSE)),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
    ]);
    let created = client(&server)
        .schedules()
        .create(create_input(application_target(), "application-job"))
        .await
        .unwrap();
    assert_eq!(created.schedule_id().as_str(), "schedule-1");
    let requests = server.finish_all();
    assert_eq!(requests.len(), 3);
    assert_request(
        &requests[1],
        "schedule.create",
        serde_json::json!({
            "name":"application-job","description":"initial","cronExpression":"0 0 * * *",
            "shellType":"bash","command":COMMAND_CANARY,"script":SCRIPT_CANARY,"enabled":false,
            "timezone":"UTC","scheduleType":"application","applicationId":"application-1"
        }),
    );

    let collision_server = TestServer::respond_with_json(list_with(APPLICATION_RESPONSE));
    let error = client(&collision_server)
        .schedules()
        .create(create_input(application_target(), "application-job"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "schedule.create"
        }
    ));
    assert_eq!(collision_server.finish_all().len(), 1);
}

#[tokio::test]
async fn schedule_create_rejects_mismatched_or_uncollected_identity() {
    let wrong_name = proof_response(&APPLICATION_RESPONSE.replace("application-job", "wrong-job"));
    let cases = [
        vec![("200 OK", "[]"), ("200 OK", wrong_name)],
        vec![
            ("200 OK", "[]"),
            ("200 OK", proof_response(APPLICATION_RESPONSE)),
            ("200 OK", "[]"),
        ],
    ];
    for responses in cases {
        let server = TestServer::respond_in_sequence(responses);
        let error = client(&server)
            .schedules()
            .create(create_input(application_target(), "application-job"))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            Error::OutcomeUnknown {
                operation: "schedule.create",
                ..
            }
        ));
        assert_no_canary(&error);
        server.finish_all();
    }

    let server = TestServer::respond_in_sequence(vec![("200 OK", "[]"), ("200 OK", "{not-json")]);
    let error = client(&server)
        .schedules()
        .create(create_input(application_target(), "application-job"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "schedule.create",
            ..
        }
    ));
    assert_no_canary(&error);
    server.finish_all();
}

#[tokio::test]
async fn schedule_create_requires_exact_new_identity_and_executable_proof() {
    let unrelated = APPLICATION_RESPONSE
        .replace("schedule-1", "schedule-2")
        .replace("application-job", "unrelated-job");
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", "[]"),
        ("200 OK", proof_response(APPLICATION_RESPONSE)),
        (
            "200 OK",
            list_with(&format!("{APPLICATION_RESPONSE},{unrelated}")),
        ),
    ]);
    let error = client(&server)
        .schedules()
        .create(create_input(application_target(), "application-job"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "schedule.create",
            ..
        }
    ));
    server.finish_all();

    let existing = APPLICATION_RESPONSE.replace("application-job", "existing-job");
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", list_with(&existing)),
        ("200 OK", proof_response(APPLICATION_RESPONSE)),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
    ]);
    let error = client(&server)
        .schedules()
        .create(create_input(application_target(), "application-job"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "schedule.create",
            ..
        }
    ));
    server.finish_all();

    for response in [
        proof_response_with_secrets(
            APPLICATION_RESPONSE,
            "wrong-command-proof-canary",
            Some(SCRIPT_CANARY),
        ),
        proof_response_with_secrets(
            APPLICATION_RESPONSE,
            COMMAND_CANARY,
            Some("wrong-script-proof-canary"),
        ),
    ] {
        let server = TestServer::respond_in_sequence(vec![("200 OK", "[]"), ("200 OK", response)]);
        let error = client(&server)
            .schedules()
            .create(create_input(application_target(), "application-job"))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            Error::OutcomeUnknown {
                operation: "schedule.create",
                ..
            }
        ));
        let rendered = format!("{error:?}");
        assert!(!rendered.contains("wrong-command-proof-canary"));
        assert!(!rendered.contains("wrong-script-proof-canary"));
        server.finish_all();
    }
}

#[tokio::test]
async fn schedule_update_and_delete_use_exact_requests() {
    let updated_response = APPLICATION_RESPONSE
        .replace("application-job", "updated-job")
        .replace("initial", "updated")
        .replace("0 0 * * *", "30 2 * * 1")
        .replace("\"bash\"", "\"sh\"")
        .replace("\"enabled\":false", "\"enabled\":true");
    let update = UpdateSchedule::new(
        ScheduleId::new("schedule-1"),
        application_target(),
        "updated-job",
        Some("updated".to_owned()),
        "30 2 * * 1",
        ShellType::Sh,
        Zeroizing::new(COMMAND_CANARY.to_owned()),
        Some(Zeroizing::new(SCRIPT_CANARY.to_owned())),
        true,
        Some("UTC".to_owned()),
    );
    assert_no_canary(&update);
    let updated_response = proof_response(&updated_response);
    let updated_list = list_with(updated_response);
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", APPLICATION_RESPONSE),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
        ("200 OK", updated_response),
        ("200 OK", updated_list),
    ]);
    client(&server).schedules().update(update).await.unwrap();
    let requests = server.finish_all();
    assert_request(
        &requests[2],
        "schedule.update",
        serde_json::json!({
            "scheduleId":"schedule-1","name":"updated-job","description":"updated",
            "cronExpression":"30 2 * * 1","shellType":"sh","command":COMMAND_CANARY,
            "script":SCRIPT_CANARY,"enabled":true,"timezone":"UTC"
        }),
    );
    assert!(requests[3].starts_with(
        "GET /api/schedule.list?id=application-1&scheduleType=application HTTP/1.1\r\n"
    ));

    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", APPLICATION_RESPONSE),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
        ("200 OK", "true"),
        ("200 OK", "[]"),
    ]);
    client(&server)
        .schedules()
        .delete(ScheduleId::new("schedule-1"), application_target())
        .await
        .unwrap();
    let requests = server.finish_all();
    assert_request(
        &requests[2],
        "schedule.delete",
        serde_json::json!({"scheduleId":"schedule-1"}),
    );
    assert!(requests[3].starts_with(
        "GET /api/schedule.list?id=application-1&scheduleType=application HTTP/1.1\r\n"
    ));
}

#[tokio::test]
async fn schedule_update_requires_safe_response_and_collection_proof() {
    let wrong_target = APPLICATION_RESPONSE
        .replace("application-job", "updated-job")
        .replace("initial", "updated")
        .replace("0 0 * * *", "30 2 * * 1")
        .replace("\"bash\"", "\"sh\"")
        .replace("\"enabled\":false", "\"enabled\":true")
        .replace("application-1", "application-2");
    for response in [proof_response(&wrong_target), "{not-json"] {
        let server = TestServer::respond_in_sequence(vec![
            ("200 OK", APPLICATION_RESPONSE),
            ("200 OK", list_with(APPLICATION_RESPONSE)),
            ("200 OK", response),
        ]);
        let error = client(&server)
            .schedules()
            .update(UpdateSchedule::new(
                ScheduleId::new("schedule-1"),
                application_target(),
                "updated-job",
                Some("updated".to_owned()),
                "30 2 * * 1",
                ShellType::Sh,
                Zeroizing::new(COMMAND_CANARY.to_owned()),
                Some(Zeroizing::new(SCRIPT_CANARY.to_owned())),
                true,
                Some("UTC".to_owned()),
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            Error::OutcomeUnknown {
                operation: "schedule.update",
                ..
            }
        ));
        assert_no_canary(&error);
        assert_eq!(server.finish_all().len(), 3);
    }

    let updated_response = APPLICATION_RESPONSE
        .replace("application-job", "updated-job")
        .replace("initial", "updated")
        .replace("0 0 * * *", "30 2 * * 1")
        .replace("\"bash\"", "\"sh\"")
        .replace("\"enabled\":false", "\"enabled\":true");
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", APPLICATION_RESPONSE),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
        ("200 OK", proof_response(&updated_response)),
        ("200 OK", "[]"),
    ]);
    let error = client(&server)
        .schedules()
        .update(UpdateSchedule::new(
            ScheduleId::new("schedule-1"),
            application_target(),
            "updated-job",
            Some("updated".to_owned()),
            "30 2 * * 1",
            ShellType::Sh,
            Zeroizing::new(COMMAND_CANARY.to_owned()),
            Some(Zeroizing::new(SCRIPT_CANARY.to_owned())),
            true,
            Some("UTC".to_owned()),
        ))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "schedule.update",
            ..
        }
    ));
    assert_no_canary(&error);
    server.finish_all();
}

#[tokio::test]
async fn schedule_update_preflight_rejects_rename_collisions_and_wrong_targets() {
    let other = APPLICATION_RESPONSE
        .replace("schedule-1", "schedule-2")
        .replace("application-job", "taken-job");
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", APPLICATION_RESPONSE),
        (
            "200 OK",
            list_with(&format!("{APPLICATION_RESPONSE},{other}")),
        ),
    ]);
    let error = client(&server)
        .schedules()
        .update(UpdateSchedule::new(
            ScheduleId::new("schedule-1"),
            application_target(),
            "taken-job",
            None,
            "0 0 * * *",
            ShellType::Bash,
            Zeroizing::new(COMMAND_CANARY.to_owned()),
            Some(Zeroizing::new(SCRIPT_CANARY.to_owned())),
            false,
            Some("UTC".to_owned()),
        ))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "schedule.update"
        }
    ));
    assert_eq!(server.finish_all().len(), 2);

    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", APPLICATION_RESPONSE),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
    ]);
    let error = client(&server)
        .schedules()
        .update(UpdateSchedule::new(
            ScheduleId::new("schedule-1"),
            compose_target(),
            "application-job",
            Some("initial".to_owned()),
            "0 0 * * *",
            ShellType::Bash,
            Zeroizing::new(COMMAND_CANARY.to_owned()),
            Some(Zeroizing::new(SCRIPT_CANARY.to_owned())),
            false,
            Some("UTC".to_owned()),
        ))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "schedule.update"
        }
    ));
    assert_eq!(server.finish_all().len(), 2);
}

#[tokio::test]
async fn schedule_update_requires_exact_command_and_script_proof() {
    let updated = APPLICATION_RESPONSE
        .replace("application-job", "updated-job")
        .replace("initial", "updated")
        .replace("0 0 * * *", "30 2 * * 1")
        .replace("\"bash\"", "\"sh\"")
        .replace("\"enabled\":false", "\"enabled\":true");
    for response in [
        proof_response_with_secrets(&updated, "wrong-command-proof-canary", Some(SCRIPT_CANARY)),
        proof_response_with_secrets(&updated, COMMAND_CANARY, Some("wrong-script-proof-canary")),
    ] {
        let server = TestServer::respond_in_sequence(vec![
            ("200 OK", APPLICATION_RESPONSE),
            ("200 OK", list_with(APPLICATION_RESPONSE)),
            ("200 OK", response),
        ]);
        let error = client(&server)
            .schedules()
            .update(UpdateSchedule::new(
                ScheduleId::new("schedule-1"),
                application_target(),
                "updated-job",
                Some("updated".to_owned()),
                "30 2 * * 1",
                ShellType::Sh,
                Zeroizing::new(COMMAND_CANARY.to_owned()),
                Some(Zeroizing::new(SCRIPT_CANARY.to_owned())),
                true,
                Some("UTC".to_owned()),
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            Error::OutcomeUnknown {
                operation: "schedule.update",
                ..
            }
        ));
        let rendered = format!("{error:?}");
        assert!(!rendered.contains("wrong-command-proof-canary"));
        assert!(!rendered.contains("wrong-script-proof-canary"));
        assert_eq!(server.finish_all().len(), 3);
    }
}

#[tokio::test]
async fn schedule_mutations_reject_privileged_schedule_identities_before_post() {
    for schedule_type in ["server", "dokploy-server"] {
        let update_server = TestServer::respond_with_json(privileged_response(schedule_type));
        let error = client(&update_server)
            .schedules()
            .update(UpdateSchedule::new(
                ScheduleId::new("schedule-1"),
                application_target(),
                "job",
                None,
                "0 0 * * *",
                ShellType::Bash,
                Zeroizing::new(COMMAND_CANARY.to_owned()),
                Some(Zeroizing::new(SCRIPT_CANARY.to_owned())),
                false,
                None,
            ))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            Error::Decode {
                operation: "schedule.one",
                ..
            }
        ));
        assert_no_canary(&error);
        assert_eq!(update_server.finish_all().len(), 1);

        let delete_server = TestServer::respond_with_json(privileged_response(schedule_type));
        let error = client(&delete_server)
            .schedules()
            .delete(ScheduleId::new("schedule-1"), application_target())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            Error::Decode {
                operation: "schedule.one",
                ..
            }
        ));
        assert_no_canary(&error);
        assert_eq!(delete_server.finish_all().len(), 1);
    }
}

#[tokio::test]
async fn schedule_delete_requires_target_agreement_and_authoritative_absence() {
    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", APPLICATION_RESPONSE),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
    ]);
    let error = client(&server)
        .schedules()
        .delete(ScheduleId::new("schedule-1"), compose_target())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::UnexpectedResponse {
            operation: "schedule.delete"
        }
    ));
    assert_eq!(server.finish_all().len(), 2);

    let server = TestServer::respond_in_sequence(vec![
        ("200 OK", APPLICATION_RESPONSE),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
        ("200 OK", "true"),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
    ]);
    let error = client(&server)
        .schedules()
        .delete(ScheduleId::new("schedule-1"), application_target())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "schedule.delete",
            ..
        }
    ));
    assert_eq!(server.finish_all().len(), 4);
}

#[tokio::test]
async fn schedule_rejects_invalid_inputs_before_transport() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .unwrap();
    let errors = [
        client
            .schedules()
            .get(ScheduleId::new(""))
            .await
            .unwrap_err(),
        client
            .schedules()
            .by_target(ScheduleTarget::Compose {
                compose_id: ComposeId::new("compose-1"),
                service_name: String::new(),
            })
            .await
            .unwrap_err(),
        client
            .schedules()
            .create(CreateSchedule::new(
                application_target(),
                "job",
                None,
                "0 0 * * *",
                ShellType::Bash,
                Zeroizing::new(String::new()),
                None,
                false,
                None,
            ))
            .await
            .unwrap_err(),
        client
            .schedules()
            .update(UpdateSchedule::new(
                ScheduleId::new("schedule-1"),
                application_target(),
                "job",
                None,
                "",
                ShellType::Bash,
                Zeroizing::new("command".to_owned()),
                None,
                false,
                None,
            ))
            .await
            .unwrap_err(),
        client
            .schedules()
            .delete(ScheduleId::new(""), application_target())
            .await
            .unwrap_err(),
        client
            .schedules()
            .delete(
                ScheduleId::new("schedule-1"),
                ScheduleTarget::Compose {
                    compose_id: ComposeId::new("compose-1"),
                    service_name: String::new(),
                },
            )
            .await
            .unwrap_err(),
    ];
    assert!(
        errors
            .iter()
            .all(|error| matches!(error, Error::InvalidRequest { .. }))
    );
}

#[tokio::test]
async fn schedule_mutations_are_single_attempt_and_secret_safe_on_unknown_outcome() {
    let create_server = TestServer::close_after_requests(vec![("200 OK", "[]")]);
    let error = client(&create_server)
        .schedules()
        .create(create_input(application_target(), "application-job"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "schedule.create",
            ..
        }
    ));
    assert_no_canary(&error);
    assert_eq!(create_server.finish_all().len(), 2);

    let update_server = TestServer::close_after_requests(vec![
        ("200 OK", APPLICATION_RESPONSE),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
    ]);
    let error = client(&update_server)
        .schedules()
        .update(UpdateSchedule::new(
            ScheduleId::new("schedule-1"),
            application_target(),
            "job",
            None,
            "0 0 * * *",
            ShellType::Bash,
            Zeroizing::new(COMMAND_CANARY.to_owned()),
            Some(Zeroizing::new(SCRIPT_CANARY.to_owned())),
            false,
            None,
        ))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "schedule.update",
            ..
        }
    ));
    assert_no_canary(&error);
    assert_eq!(update_server.finish_all().len(), 3);

    let delete_server = TestServer::close_after_requests(vec![
        ("200 OK", APPLICATION_RESPONSE),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
    ]);
    let error = client(&delete_server)
        .schedules()
        .delete(ScheduleId::new("schedule-1"), application_target())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::OutcomeUnknown {
            operation: "schedule.delete",
            ..
        }
    ));
    assert_eq!(delete_server.finish_all().len(), 3);
}

#[tokio::test]
async fn schedule_mutation_rejections_never_retain_echoed_commands_or_scripts() {
    let echoed = r#"{
        "code":"ECHOED_COMMAND",
        "message":"schedule-command-canary-do-not-leak",
        "issues":[{"message":"schedule-script-canary-do-not-leak"}]
    }"#;
    let create_server =
        TestServer::respond_in_sequence(vec![("200 OK", "[]"), ("400 Bad Request", echoed)]);
    let error = client(&create_server)
        .schedules()
        .create(create_input(application_target(), "application-job"))
        .await
        .unwrap_err();
    let dokploy = error.dokploy().expect("HTTP status remains structured");
    assert_eq!(dokploy.status(), 400);
    assert_eq!(dokploy.code(), "BAD_REQUEST");
    assert_eq!(dokploy.message(), "Bad Request");
    assert!(dokploy.issues().is_empty());
    assert_no_canary(&error);
    assert_eq!(create_server.finish_all().len(), 2);

    let update_server = TestServer::respond_in_sequence(vec![
        ("200 OK", APPLICATION_RESPONSE),
        ("200 OK", list_with(APPLICATION_RESPONSE)),
        ("422 Unprocessable Entity", echoed),
    ]);
    let error = client(&update_server)
        .schedules()
        .update(UpdateSchedule::new(
            ScheduleId::new("schedule-1"),
            application_target(),
            "job",
            None,
            "0 0 * * *",
            ShellType::Bash,
            Zeroizing::new(COMMAND_CANARY.to_owned()),
            Some(Zeroizing::new(SCRIPT_CANARY.to_owned())),
            false,
            None,
        ))
        .await
        .unwrap_err();
    let dokploy = error.dokploy().expect("HTTP status remains structured");
    assert_eq!(dokploy.status(), 422);
    assert_eq!(dokploy.code(), "UNPROCESSABLE_ENTITY");
    assert_eq!(dokploy.message(), "Unprocessable Entity");
    assert!(dokploy.issues().is_empty());
    assert_no_canary(&error);
    assert_eq!(update_server.finish_all().len(), 3);
}

#[tokio::test]
async fn compose_wide_list_spans_services_and_is_bounded_and_owner_consistent() {
    let other_service = COMPOSE_RESPONSE
        .replace("schedule-2", "schedule-3")
        .replace("\"serviceName\":\"worker\"", "\"serviceName\":\"web\"");
    let server =
        TestServer::respond_with_json(list_with(&format!("{COMPOSE_RESPONSE},{other_service}")));
    let collection = client(&server)
        .schedules()
        .by_compose(ComposeId::new("compose-1"))
        .await
        .unwrap();
    assert_eq!(collection.compose_id(), &ComposeId::new("compose-1"));
    assert_eq!(collection.schedules().len(), 2);
    let services = collection
        .schedules()
        .iter()
        .map(|schedule| match &schedule.target {
            ScheduleTarget::Compose { service_name, .. } => service_name.as_str(),
            ScheduleTarget::Application(_) => panic!("Compose collections hold Compose targets"),
        })
        .collect::<Vec<_>>();
    assert_eq!(services, ["worker", "web"]);
    assert_no_canary(&collection);
    assert!(
        server
            .finish()
            .contains("id=compose-1&scheduleType=compose")
    );

    // The same name on two services is distinct; per-target reads would reject it.
    let per_target =
        TestServer::respond_with_json(list_with(&format!("{COMPOSE_RESPONSE},{other_service}")));
    assert!(matches!(
        client(&per_target)
            .schedules()
            .by_target(compose_target())
            .await
            .unwrap_err(),
        Error::UnexpectedResponse {
            operation: "schedule.list"
        }
    ));
    per_target.finish();

    let duplicate_in_service = COMPOSE_RESPONSE.replace("schedule-2", "schedule-3");
    let duplicate_id = COMPOSE_RESPONSE.replace("compose-job", "other-job");
    let other_compose = COMPOSE_RESPONSE
        .replace("schedule-2", "schedule-3")
        .replace("compose-job", "other-job")
        .replace("\"composeId\":\"compose-1\"", "\"composeId\":\"compose-2\"");
    let too_many = (0..10_001)
        .map(|index| {
            COMPOSE_RESPONSE
                .replace("schedule-2", &format!("schedule-{index}"))
                .replace("compose-job", &format!("job-{index}"))
        })
        .collect::<Vec<_>>()
        .join(",");
    for response in [
        list_with(&format!("{COMPOSE_RESPONSE},{duplicate_in_service}")),
        list_with(&format!("{COMPOSE_RESPONSE},{duplicate_id}")),
        list_with(&format!("{COMPOSE_RESPONSE},{other_compose}")),
        list_with(&format!("{COMPOSE_RESPONSE},{APPLICATION_RESPONSE}")),
        list_with(&too_many),
    ] {
        let server = TestServer::respond_with_json(response);
        let error = client(&server)
            .schedules()
            .by_compose(ComposeId::new("compose-1"))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            Error::UnexpectedResponse {
                operation: "schedule.list"
            }
        ));
        assert_no_canary(&error);
        server.finish();
    }
}

#[tokio::test]
async fn compose_wide_list_rejects_an_empty_compose_before_transport() {
    let client = Dokploy::builder()
        .url("http://127.0.0.1:9")
        .api_key("test-api-key")
        .build()
        .unwrap();

    assert!(matches!(
        client
            .schedules()
            .by_compose(ComposeId::new(""))
            .await
            .unwrap_err(),
        Error::InvalidRequest { .. }
    ));
}
