use dokploy_sdk::ProjectTopology;

#[test]
fn project_all_accepts_sparse_libsql_identifiers_without_weakening_project_one_rows() {
    let body =
        include_str!("../../../fixtures/api/live/v0.30.6/project-all.libsql-sparse.owner.json");

    let topology: ProjectTopology =
        serde_json::from_str(body).expect("sparse project.all LibSQL rows decode");
    let environment = &topology.projects()[0].environments[0];
    let database = &environment.libsql[0];

    assert_eq!(database.libsql_id.as_str(), "libsql-1");
    assert_eq!(database.name, None);
    assert_eq!(database.app_name, None);
}
