use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use dokploy_sdk::{
    ApplicationDetails, ApplicationId, ComposeDetails, CreateApplication, CreateCompose,
    CreateLibSql, CreateMariaDb, CreateMongo, CreateMySql, CreatePostgres, CreateRedis,
    EnvironmentId, LibSqlDetails, LibSqlNode, MariaDbDetails, MongoDetails, MySqlDetails, Nullable,
    PostgresDetails, ProjectId, RedisDetails, RegistryId, ResponseField, ServerId, ServerPlacement,
    UpdateApplication,
};

fn password() -> Zeroizing<String> {
    Zeroizing::new("safe-test-password".to_owned())
}

fn application() -> CreateApplication {
    CreateApplication::new("api", EnvironmentId::new("environment-1"))
}

fn compose() -> CreateCompose {
    CreateCompose::new(
        "compose",
        EnvironmentId::new("environment-1"),
        Zeroizing::new("services: {}".to_owned()),
    )
}

fn postgres() -> CreatePostgres {
    CreatePostgres::new(
        "postgres",
        EnvironmentId::new("environment-1"),
        "app",
        "app",
        password(),
    )
}

fn mysql() -> CreateMySql {
    CreateMySql::new(
        "mysql",
        EnvironmentId::new("environment-1"),
        "app",
        "app",
        password(),
        password(),
    )
}

fn mariadb() -> CreateMariaDb {
    CreateMariaDb::new(
        "mariadb",
        EnvironmentId::new("environment-1"),
        "app",
        "app",
        password(),
    )
}

fn mongo() -> CreateMongo {
    CreateMongo::new(
        "mongo",
        EnvironmentId::new("environment-1"),
        "app",
        password(),
    )
}

fn libsql() -> CreateLibSql {
    CreateLibSql::new(
        "libsql",
        "libsql",
        ProjectId::new("project-1"),
        EnvironmentId::new("environment-1"),
        "app",
        password(),
        LibSqlNode::Primary,
    )
}

fn redis() -> CreateRedis {
    CreateRedis::new("redis", EnvironmentId::new("environment-1"), password())
}

fn assert_placement(unmanaged: impl Serialize, local: impl Serialize, external: impl Serialize) {
    let unmanaged = serde_json::to_value(unmanaged).expect("unmanaged input serializes");
    let local = serde_json::to_value(local).expect("local input serializes");
    let external = serde_json::to_value(external).expect("external input serializes");

    assert!(!unmanaged.as_object().unwrap().contains_key("serverId"));
    assert_eq!(local["serverId"], Value::Null);
    assert_eq!(external["serverId"], "server-1");
}

#[test]
fn every_supported_create_distinguishes_unmanaged_local_and_external_placement() {
    assert_placement(
        application(),
        application().with_server_placement(ServerPlacement::Local),
        application().with_server_placement(ServerPlacement::Server(ServerId::new("server-1"))),
    );
    assert_placement(
        compose(),
        compose().with_server_placement(ServerPlacement::Local),
        compose().with_server_placement(ServerPlacement::Server(ServerId::new("server-1"))),
    );
    assert_placement(
        postgres(),
        postgres().with_server_placement(ServerPlacement::Local),
        postgres().with_server_placement(ServerPlacement::Server(ServerId::new("server-1"))),
    );
    assert_placement(
        mysql(),
        mysql().with_server_placement(ServerPlacement::Local),
        mysql().with_server_placement(ServerPlacement::Server(ServerId::new("server-1"))),
    );
    assert_placement(
        mariadb(),
        mariadb().with_server_placement(ServerPlacement::Local),
        mariadb().with_server_placement(ServerPlacement::Server(ServerId::new("server-1"))),
    );
    assert_placement(
        mongo(),
        mongo().with_server_placement(ServerPlacement::Local),
        mongo().with_server_placement(ServerPlacement::Server(ServerId::new("server-1"))),
    );
    assert_placement(
        libsql(),
        libsql().with_server_placement(ServerPlacement::Local),
        libsql().with_server_placement(ServerPlacement::Server(ServerId::new("server-1"))),
    );
    assert_placement(
        redis(),
        redis().with_server_placement(ServerPlacement::Local),
        redis().with_server_placement(ServerPlacement::Server(ServerId::new("server-1"))),
    );
}

fn assert_server_presence<T>(fixture: &str, field: impl Fn(&T) -> &ResponseField<ServerId>)
where
    T: DeserializeOwned,
{
    let mut value: Value = serde_json::from_str(fixture).expect("fixture is JSON");
    value.as_object_mut().unwrap().remove("serverId");
    let omitted: T = serde_json::from_value(value.clone()).expect("omitted field decodes");
    assert_eq!(field(&omitted), &ResponseField::NotReturned);

    value["serverId"] = Value::Null;
    let local: T = serde_json::from_value(value.clone()).expect("null field decodes");
    assert_eq!(field(&local), &ResponseField::Null);

    value["serverId"] = json!("server-1");
    let external: T = serde_json::from_value(value).expect("server field decodes");
    assert_eq!(
        field(&external),
        &ResponseField::Value(ServerId::new("server-1"))
    );
}

#[test]
fn v0306_service_fixtures_support_presence_aware_primary_server_observation() {
    assert_server_presence::<ApplicationDetails>(
        include_str!("../../../fixtures/api/live/v0.30.6/application-one.owner.json"),
        |details| &details.server_id,
    );
    assert_server_presence::<ComposeDetails>(
        include_str!("../../../fixtures/api/live/v0.30.6/compose-one.created.owner.json"),
        |details| &details.server_id,
    );
    assert_server_presence::<PostgresDetails>(
        include_str!("../../../fixtures/api/live/v0.30.6/postgres-one.owner.json"),
        |details| &details.server_id,
    );
    assert_server_presence::<MySqlDetails>(
        include_str!("../../../fixtures/api/live/v0.30.6/mysql-one.created.owner.json"),
        |details| &details.server_id,
    );
    assert_server_presence::<MariaDbDetails>(
        include_str!("../../../fixtures/api/live/v0.30.6/mariadb-one.created.owner.json"),
        |details| &details.server_id,
    );
    assert_server_presence::<MongoDetails>(
        include_str!("../../../fixtures/api/live/v0.30.6/mongo-one.created.owner.json"),
        |details| &details.server_id,
    );
    assert_server_presence::<LibSqlDetails>(
        include_str!("../../../fixtures/api/live/v0.30.6/libsql-one.created.owner.json"),
        |details| &details.server_id,
    );
    assert_server_presence::<RedisDetails>(
        include_str!("../../../fixtures/api/live/v0.30.6/redis-one.owner.json"),
        |details| &details.server_id,
    );
}

#[test]
fn application_associations_are_presence_aware_and_nested_secrets_are_discarded() {
    let application: ApplicationDetails = serde_json::from_value(json!({
        "applicationId": "application-1",
        "environmentId": "environment-1",
        "name": "API",
        "appName": "api",
        "serverId": null,
        "buildServerId": "build-server-1",
        "registryId": "registry-1",
        "buildRegistryId": null,
        "server": {"command": "secret-canary"},
        "registry": {"password": "secret-canary"}
    }))
    .expect("application associations decode");

    assert_eq!(application.server_id, ResponseField::Null);
    assert_eq!(
        application.build_server_id,
        ResponseField::Value(ServerId::new("build-server-1"))
    );
    assert_eq!(
        application.registry_id,
        ResponseField::Value(RegistryId::new("registry-1"))
    );
    assert_eq!(application.build_registry_id, ResponseField::Null);
    assert_eq!(application.rollback_registry_id, ResponseField::NotReturned);
    assert!(!format!("{application:?}").contains("secret-canary"));
}

#[test]
fn application_update_serializes_only_explicit_association_intent() {
    let update = UpdateApplication::new(ApplicationId::new("application-1"))
        .with_build_server(Nullable::Value(ServerId::new("build-server-1")))
        .with_registry(Nullable::Null)
        .with_build_registry(Nullable::Value(RegistryId::new("build-registry-1")))
        .with_rollback_registry(Nullable::Null);

    assert_eq!(
        serde_json::to_value(update).expect("application update serializes"),
        json!({
            "applicationId": "application-1",
            "buildServerId": "build-server-1",
            "registryId": null,
            "buildRegistryId": "build-registry-1",
            "rollbackRegistryId": null
        })
    );
}
