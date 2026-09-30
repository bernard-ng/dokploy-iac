use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use clap::Parser;
use dokploy_cli::cli::Cli;
use dokploy_cli::cli::ImportKind;
use dokploy_cli::config::ConfigRepository;
use dokploy_cli::credentials::{ApiKey, CredentialStore, CredentialStoreError};
use dokploy_cli::execute_with_input;
use dokploy_cli::import::{
    ImportError, ImportPrompter, ImportRequest, import_resource, select_with_prompter,
};
use dokploy_cli::planning::plan_workspace;
use dokploy_config::{DokployConfig, Field};
use dokploy_sdk::Dokploy;
use dokploy_state::{InstanceIdentity, StateStore};

struct TestServer {
    url: String,
    requests: Receiver<Vec<String>>,
    thread: JoinHandle<()>,
}

struct NoCredentials;

struct FakePrompter {
    selected: usize,
    address: String,
    choices: Vec<String>,
}

impl ImportPrompter for FakePrompter {
    fn select(&mut self, choices: &[String]) -> Result<usize, ImportError> {
        self.choices = choices.to_vec();
        Ok(self.selected)
    }

    fn address(&mut self, default: &str) -> Result<String, ImportError> {
        Ok(if self.address.is_empty() {
            default.to_owned()
        } else {
            self.address.clone()
        })
    }
}

impl CredentialStore for NoCredentials {
    fn get(&self, _context: &str) -> Result<Option<ApiKey>, CredentialStoreError> {
        Ok(None)
    }

    fn set(&self, _context: &str, _api_key: &ApiKey) -> Result<(), CredentialStoreError> {
        Ok(())
    }

    fn delete(&self, _context: &str) -> Result<(), CredentialStoreError> {
        Ok(())
    }
}

#[tokio::test]
async fn interactive_discovery_lists_remote_resources_and_slugs_the_default_address() {
    let topology = r#"[{"projectId":"project-1","name":"IaC Contract Test","environments":[]}]"#;
    let server = TestServer::respond_in_sequence(vec![topology]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .unwrap();
    let mut prompter = FakePrompter {
        selected: 0,
        address: String::new(),
        choices: Vec::new(),
    };

    let selection = select_with_prompter(
        &client,
        std::path::PathBuf::from("dokploy.yaml"),
        &mut prompter,
    )
    .await
    .expect("interactive selection succeeds");

    assert_eq!(selection.kind, ImportKind::Project);
    assert_eq!(selection.address.to_string(), "project.iac-contract-test");
    assert!(prompter.choices[0].contains("IaC Contract Test"));
    assert!(server.finish()[0].starts_with("GET /api/project.all"));
}

#[tokio::test]
async fn interactive_discovery_distinguishes_duplicate_names_by_remote_identity() {
    let topology = r#"[{"projectId":"project-1","name":"Platform","environments":[]},{"projectId":"project-2","name":"Platform","environments":[]}]"#;
    let server = TestServer::respond_in_sequence(vec![topology]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .unwrap();
    let mut prompter = FakePrompter {
        selected: 1,
        address: "project.second".to_owned(),
        choices: Vec::new(),
    };

    let selection = select_with_prompter(
        &client,
        std::path::PathBuf::from("dokploy.yaml"),
        &mut prompter,
    )
    .await
    .expect("the second duplicate name remains selectable");

    assert_eq!(selection.remote_id, "project-2");
    assert_ne!(prompter.choices[0], prompter.choices[1]);
    assert!(prompter.choices[1].contains("project-2"));
    server.finish();
}

#[tokio::test]
async fn interactive_import_fails_before_discovery_without_a_terminal() {
    let workspace = tempfile::tempdir().unwrap();
    let repository = ConfigRepository::new(workspace.path().join("config.toml"));
    let cli = Cli::try_parse_from([
        "dokploy",
        "--url",
        "http://127.0.0.1:1",
        "--api-key",
        "test-key",
        "import",
    ])
    .unwrap();
    let mut input = std::io::empty();
    let mut output = Vec::new();

    let error = execute_with_input(cli, &repository, &NoCredentials, &mut input, &mut output)
        .await
        .expect_err("non-terminal interactive import is rejected");

    assert!(error.to_string().contains("requires a terminal"));
}

#[tokio::test]
async fn direct_import_dispatches_through_the_public_cli_without_remote_mutation() {
    let project = r#"{"projectId":"project-1","name":"Remote Project","environments":[]}"#;
    let server = TestServer::respond_in_sequence(vec![project]);
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");
    let repository = ConfigRepository::new(workspace.path().join("config.toml"));
    let cli = Cli::try_parse_from([
        "dokploy",
        "--url",
        &server.url,
        "--api-key",
        "test-key",
        "import",
        "project",
        "project-1",
        "--as",
        "project.imported",
        "--file",
        config_file.to_str().unwrap(),
    ])
    .unwrap();
    let mut input = std::io::empty();
    let mut output = Vec::new();

    execute_with_input(cli, &repository, &NoCredentials, &mut input, &mut output)
        .await
        .expect("direct import dispatch succeeds");

    assert!(config_file.exists());
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("1 resource(s) tracked")
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /api/project.one?projectId=project-1"));
}

impl TestServer {
    fn respond_in_sequence(bodies: Vec<&'static str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        let address = listener.local_addr().expect("test server has an address");
        let (sender, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut captured = Vec::new();
            for body in bodies {
                let (mut stream, _) = listener.accept().expect("test server accepts a request");
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 2048];
                loop {
                    let count = stream.read(&mut buffer).expect("request is readable");
                    bytes.extend_from_slice(&buffer[..count]);
                    if count == 0 || bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                captured.push(String::from_utf8(bytes).expect("request is UTF-8"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("response is writable");
            }
            sender.send(captured).expect("requests are returned");
        });

        Self {
            url: format!("http://{address}"),
            requests,
            thread,
        }
    }

    fn finish(self) -> Vec<String> {
        let requests = self.requests.recv().expect("requests are received");
        self.thread.join().expect("server exits");
        requests
    }
}

#[tokio::test]
async fn postgres_import_is_read_only_protected_secret_free_and_immediately_convergent() {
    let postgres = r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"Remote database","appName":"remote-db","dockerImage":"postgres:18","databaseName":"app","databaseUser":"app"}"#;
    let environment = r#"{"environmentId":"environment-1","name":"production","description":"Production","projectId":"project-1"}"#;
    let project =
        r#"{"projectId":"project-1","name":"platform","description":"Platform","environments":[]}"#;
    let project_topology = r#"[{"projectId":"project-1","name":"platform","description":"Platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"applications":[],"postgres":[{"postgresId":"postgres-1","name":"Remote database"}],"redis":[]}]}]"#;
    let environment_collection =
        r#"[{"environmentId":"environment-1","name":"production","description":"Production"}]"#;
    let postgres_collection = r#"{"items":[{"postgresId":"postgres-1","environmentId":"environment-1","name":"Remote database"}],"total":1}"#;
    let server = TestServer::respond_in_sequence(vec![
        postgres,
        environment,
        project,
        project_topology,
        environment_collection,
        environment,
        postgres_collection,
        postgres,
    ]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client is valid");
    let workspace = tempfile::tempdir().expect("workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");

    let count = import_resource(
        &client,
        ImportRequest {
            kind: ImportKind::Postgres,
            remote_id: "postgres-1".to_owned(),
            address: "postgres.main".parse().unwrap(),
            config_file: config_file.clone(),
        },
    )
    .await
    .expect("database imports");
    let plan = plan_workspace(&client, &config_file)
        .await
        .expect("fresh plan succeeds");

    assert_eq!(count, 3);
    assert!(plan.complete());
    assert!(plan.applyable());
    assert!(plan.changes().is_empty());
    let source = std::fs::read_to_string(&config_file).expect("config is readable");
    assert!(!source.contains("password"));
    let config = DokployConfig::parse(&source).expect("config is canonical and valid");
    let resource = config.resource(&"postgres.main".parse().unwrap()).unwrap();
    let database = resource.as_postgres().unwrap();
    assert_eq!(database.password(), &Field::Unmanaged);
    assert_eq!(resource.lifecycle().protect(), &Field::Set(true));
    let instance = InstanceIdentity::parse(&server.url).unwrap();
    let state = StateStore::new(workspace.path(), instance)
        .unwrap()
        .inspect()
        .unwrap()
        .unwrap();
    assert_eq!(
        state.serial(),
        0,
        "import is one absent-to-populated checkpoint"
    );
    assert!(
        state
            .resource(&"postgres.main".parse().unwrap())
            .unwrap()
            .is_protected()
    );
    assert_eq!(
        state
            .resource(&"postgres.main".parse().unwrap())
            .unwrap()
            .sensitive_inputs()
            .paths()
            .count(),
        0
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

#[tokio::test]
async fn mysql_import_is_protected_two_secret_free_and_immediately_convergent() {
    let mysql = r#"{"mysqlId":"mysql-1","environmentId":"environment-1","name":"Remote database","appName":"remote-db","dockerImage":"mysql:8","databaseName":"app","databaseUser":"app","databasePassword":"user-canary","databaseRootPassword":"root-canary"}"#;
    let environment = r#"{"environmentId":"environment-1","name":"production","description":"Production","projectId":"project-1"}"#;
    let project =
        r#"{"projectId":"project-1","name":"platform","description":"Platform","environments":[]}"#;
    let project_topology = r#"[{"projectId":"project-1","name":"platform","description":"Platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"applications":[],"postgres":[],"redis":[]}]}]"#;
    let environment_collection =
        r#"[{"environmentId":"environment-1","name":"production","description":"Production"}]"#;
    let mysql_collection = r#"{"items":[{"mysqlId":"mysql-1","environmentId":"environment-1","name":"Remote database"}],"total":1}"#;
    let server = TestServer::respond_in_sequence(vec![
        mysql,
        environment,
        project,
        project_topology,
        environment_collection,
        environment,
        mysql_collection,
        mysql,
    ]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client is valid");
    let workspace = tempfile::tempdir().expect("workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");

    let count = import_resource(
        &client,
        ImportRequest {
            kind: ImportKind::MySql,
            remote_id: "mysql-1".to_owned(),
            address: "mysql.main".parse().unwrap(),
            config_file: config_file.clone(),
        },
    )
    .await
    .expect("database imports");
    let plan = plan_workspace(&client, &config_file)
        .await
        .expect("fresh plan succeeds");

    assert_eq!(count, 3);
    assert!(plan.complete());
    assert!(plan.applyable());
    assert!(plan.changes().is_empty());
    let source = std::fs::read_to_string(&config_file).expect("config is readable");
    assert!(!source.contains("password"));
    assert!(!source.contains("user-canary"));
    assert!(!source.contains("root-canary"));
    let config = DokployConfig::parse(&source).expect("config is canonical and valid");
    let resource = config.resource(&"mysql.main".parse().unwrap()).unwrap();
    let database = resource.as_mysql().unwrap();
    assert_eq!(database.password(), &Field::Unmanaged);
    assert_eq!(database.root_password(), &Field::Unmanaged);
    assert_eq!(resource.lifecycle().protect(), &Field::Set(true));
    let instance = InstanceIdentity::parse(&server.url).unwrap();
    let state = StateStore::new(workspace.path(), instance)
        .unwrap()
        .inspect()
        .unwrap()
        .unwrap();
    assert!(
        state
            .resource(&"mysql.main".parse().unwrap())
            .unwrap()
            .is_protected()
    );
    assert_eq!(
        state
            .resource(&"mysql.main".parse().unwrap())
            .unwrap()
            .sensitive_inputs()
            .paths()
            .count(),
        0
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

#[tokio::test]
async fn mariadb_import_is_protected_two_secret_free_and_immediately_convergent() {
    let mariadb = r#"{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"Remote database","appName":"remote-db","dockerImage":"mariadb:11","databaseName":"app","databaseUser":"app","databasePassword":"user-canary","databaseRootPassword":"root-canary"}"#;
    let environment = r#"{"environmentId":"environment-1","name":"production","description":"Production","projectId":"project-1"}"#;
    let project =
        r#"{"projectId":"project-1","name":"platform","description":"Platform","environments":[]}"#;
    let project_topology = r#"[{"projectId":"project-1","name":"platform","description":"Platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"applications":[],"postgres":[],"redis":[]}]}]"#;
    let environment_collection =
        r#"[{"environmentId":"environment-1","name":"production","description":"Production"}]"#;
    let mariadb_collection = r#"{"items":[{"mariadbId":"mariadb-1","environmentId":"environment-1","name":"Remote database"}],"total":1}"#;
    let server = TestServer::respond_in_sequence(vec![
        mariadb,
        environment,
        project,
        project_topology,
        environment_collection,
        environment,
        mariadb_collection,
        mariadb,
    ]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client is valid");
    let workspace = tempfile::tempdir().expect("workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");

    let count = import_resource(
        &client,
        ImportRequest {
            kind: ImportKind::MariaDb,
            remote_id: "mariadb-1".to_owned(),
            address: "mariadb.main".parse().unwrap(),
            config_file: config_file.clone(),
        },
    )
    .await
    .expect("database imports");
    let plan = plan_workspace(&client, &config_file)
        .await
        .expect("fresh plan succeeds");

    assert_eq!(count, 3);
    assert!(plan.complete());
    assert!(plan.applyable());
    assert!(plan.changes().is_empty());
    let source = std::fs::read_to_string(&config_file).expect("config is readable");
    assert!(!source.contains("password"));
    assert!(!source.contains("user-canary"));
    assert!(!source.contains("root-canary"));
    let config = DokployConfig::parse(&source).expect("config is canonical and valid");
    let resource = config.resource(&"mariadb.main".parse().unwrap()).unwrap();
    let database = resource.as_mariadb().unwrap();
    assert_eq!(database.password(), &Field::Unmanaged);
    assert_eq!(database.root_password(), &Field::Unmanaged);
    assert_eq!(resource.lifecycle().protect(), &Field::Set(true));
    let instance = InstanceIdentity::parse(&server.url).unwrap();
    let state = StateStore::new(workspace.path(), instance)
        .unwrap()
        .inspect()
        .unwrap()
        .unwrap();
    assert!(
        state
            .resource(&"mariadb.main".parse().unwrap())
            .unwrap()
            .is_protected()
    );
    assert_eq!(
        state
            .resource(&"mariadb.main".parse().unwrap())
            .unwrap()
            .sensitive_inputs()
            .paths()
            .count(),
        0
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

#[tokio::test]
async fn redis_import_is_protected_secret_free_and_immediately_convergent() {
    let redis = r#"{"redisId":"redis-1","environmentId":"environment-1","name":"Remote cache","appName":"remote-cache","dockerImage":"redis:8"}"#;
    let environment = r#"{"environmentId":"environment-1","name":"production","description":"Production","projectId":"project-1"}"#;
    let project =
        r#"{"projectId":"project-1","name":"platform","description":"Platform","environments":[]}"#;
    let project_topology = r#"[{"projectId":"project-1","name":"platform","description":"Platform","environments":[{"environmentId":"environment-1","name":"production","isDefault":true,"applications":[],"postgres":[],"redis":[{"redisId":"redis-1","name":"Remote cache"}]}]}]"#;
    let environment_collection =
        r#"[{"environmentId":"environment-1","name":"production","description":"Production"}]"#;
    let redis_collection = r#"{"items":[{"redisId":"redis-1","environmentId":"environment-1","name":"Remote cache"}],"total":1}"#;
    let server = TestServer::respond_in_sequence(vec![
        redis,
        environment,
        project,
        project_topology,
        environment_collection,
        environment,
        redis_collection,
        redis,
    ]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .expect("client is valid");
    let workspace = tempfile::tempdir().expect("workspace is available");
    let config_file = workspace.path().join("dokploy.yaml");

    import_resource(
        &client,
        ImportRequest {
            kind: ImportKind::Redis,
            remote_id: "redis-1".to_owned(),
            address: "redis.cache".parse().unwrap(),
            config_file: config_file.clone(),
        },
    )
    .await
    .expect("cache imports");
    let plan = plan_workspace(&client, &config_file)
        .await
        .expect("fresh plan succeeds");

    assert!(plan.changes().is_empty());
    let source = std::fs::read_to_string(&config_file).expect("config is readable");
    assert!(!source.contains("password"));
    let config = DokployConfig::parse(&source).expect("config is canonical and valid");
    let resource = config.resource(&"redis.cache".parse().unwrap()).unwrap();
    assert_eq!(resource.as_redis().unwrap().password(), &Field::Unmanaged);
    assert_eq!(resource.lifecycle().protect(), &Field::Set(true));
    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

#[tokio::test]
async fn domain_import_tracks_its_application_dependency_and_immediately_converges() {
    let domain =
        r#"{"domainId":"domain-1","host":"api.example.test","applicationId":"application-1"}"#;
    let application = r#"{"applicationId":"application-1","name":"API Service","appName":"api","environmentId":"environment-1","description":"API","replicas":1}"#;
    let environment =
        r#"{"environmentId":"environment-1","name":"Production West","projectId":"project-1"}"#;
    let project = r#"{"projectId":"project-1","name":"IaC Contract Test","environments":[]}"#;
    let project_topology = r#"[{"projectId":"project-1","name":"IaC Contract Test","environments":[{"environmentId":"environment-1","name":"Production West","isDefault":true,"applications":[{"applicationId":"application-1","name":"API Service"}],"postgres":[],"redis":[]}]}]"#;
    let environment_collection = r#"[{"environmentId":"environment-1","name":"Production West"}]"#;
    let application_collection = r#"{"items":[{"applicationId":"application-1","environmentId":"environment-1","name":"API Service"}],"total":1}"#;
    let domain_collection =
        r#"[{"domainId":"domain-1","host":"api.example.test","applicationId":"application-1"}]"#;
    let server = TestServer::respond_in_sequence(vec![
        domain,
        application,
        environment,
        project,
        project_topology,
        environment_collection,
        environment,
        application_collection,
        application,
        domain_collection,
        domain,
    ]);
    let client = Dokploy::builder()
        .url(&server.url)
        .api_key("test-api-key")
        .build()
        .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let config_file = workspace.path().join("dokploy.yaml");

    import_resource(
        &client,
        ImportRequest {
            kind: ImportKind::Domain,
            remote_id: "domain-1".to_owned(),
            address: "domain.public".parse().unwrap(),
            config_file: config_file.clone(),
        },
    )
    .await
    .expect("domain imports");
    let plan = plan_workspace(&client, &config_file)
        .await
        .expect("fresh plan succeeds");

    assert!(plan.changes().is_empty());
    let instance = InstanceIdentity::parse(&server.url).unwrap();
    let state = StateStore::new(workspace.path(), instance)
        .unwrap()
        .inspect()
        .unwrap()
        .unwrap();
    let imported = state.resource(&"domain.public".parse().unwrap()).unwrap();
    assert_eq!(
        imported
            .dependencies()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["application.api-service"]
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 11);
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}
