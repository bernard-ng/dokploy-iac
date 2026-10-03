mod common;

use common::{repository, widget};
use dokploy_model::{
    DiagnosticCode, Diagnostics, Document, EnvValue, Root, Selector, Source, Value,
};

fn parse(source: &str) -> Result<Document, Diagnostics> {
    Document::parse(source, repository())
}

fn parse_widget(source: &str) -> Result<Document, Diagnostics> {
    Document::parse(source, widget())
}

fn codes(result: Result<Document, Diagnostics>) -> Vec<(DiagnosticCode, String)> {
    result
        .expect_err("the document is invalid")
        .iter()
        .map(|d| (d.code, d.path.clone()))
        .collect()
}

fn settings_resource<'a>(
    document: &'a Document,
    section: &str,
    key: &str,
) -> &'a dokploy_model::Resource {
    let Root::Settings(sections) = &document.root else {
        panic!("not a settings document");
    };
    &sections[section][key]
}

const REGISTRY: &str = "\
version: 2
settings:
  registries:
    ghcr:
      type: cloud
      url: ghcr.io
      username: ci
      password:
        env: GHCR_TOKEN
      image_prefix: acme
      server:
        name: edge-1
";

#[test]
fn a_settings_document_with_a_registry_parses_into_typed_values() {
    let document = parse(REGISTRY).expect("valid");
    let ghcr = settings_resource(&document, "registries", "ghcr");
    assert_eq!(
        (ghcr.kind.as_str(), ghcr.key.as_str()),
        ("registry", "ghcr")
    );
    assert_eq!(ghcr.fields["url"].value, Value::Text("ghcr.io".into()));
    assert_eq!(
        ghcr.fields["password"].value,
        Value::Source(Source::Env("GHCR_TOKEN".into()))
    );
    assert_eq!(
        ghcr.fields["server"].value,
        Value::Selector(Selector::Name("edge-1".into()))
    );
    assert_eq!(
        ghcr.fields["url"].span.line, 6,
        "fields remember their position"
    );
    assert_eq!(document.resources().len(), 1);
}

const PROJECT: &str = "\
version: 2
project:
  slug: shop
  description: The shop
  lifecycle:
    protect: true
  environments:
    staging:
      applications:
        api:
          replicas: 2
          args: [serve, --quiet]
          server:
            local: true
          source:
            type: docker
            image: nginx
            username: ci
            password:
              env: REGISTRY_PASSWORD
          environment:
            LOG_LEVEL: info
            PORT:
              value: \"8080\"
            DATABASE_URL:
              secret:
                env: DATABASE_URL
            MAIL_KEY:
              vault:
                provider: infisical
                secret: MAIL_KEY
          depends_on: [postgres.main]
          lifecycle:
            ignore_changes: [replicas]
          redirects:
            www:
              regex: ^/old
              replacement: /new
              permanent: true
";

#[test]
fn a_project_document_nests_children_inside_their_parent() {
    let document = parse(PROJECT).expect("valid");
    let Root::Project(project) = &document.root else {
        panic!("not a project document");
    };
    assert_eq!(project.key, "shop");
    assert_eq!(project.lifecycle.protect, Some(true));
    let api = &project.children["environments"]["staging"].children["applications"]["api"];
    assert_eq!(api.fields["replicas"].value, Value::Int(2));
    assert_eq!(api.fields["server"].value, Value::Selector(Selector::Local));
    assert_eq!(api.depends_on, ["postgres.main"]);
    assert_eq!(api.lifecycle.ignore_changes, ["replicas"]);

    let Value::Env(variables) = &api.fields["environment"].value else {
        panic!("environment is an env block");
    };
    assert_eq!(variables["LOG_LEVEL"], EnvValue::Public("info".into()));
    assert_eq!(variables["PORT"], EnvValue::Public("8080".into()));
    assert_eq!(
        variables["DATABASE_URL"],
        EnvValue::Secret(Source::Env("DATABASE_URL".into()))
    );
    assert!(matches!(
        variables["MAIL_KEY"],
        EnvValue::Secret(Source::Vault { .. })
    ));
    let Value::Union { tag, fields } = &api.fields["source"].value else {
        panic!("source is a union");
    };
    assert_eq!(tag, "docker");
    assert_eq!(fields["image"], Value::Text("nginx".into()));
    assert!(matches!(fields["password"], Value::Source(Source::Env(_))));

    let redirect = &api.children["redirects"]["www"];
    assert_eq!(redirect.fields["permanent"].value, Value::Bool(true));
    assert_eq!(document.resources().len(), 4, "parents before children");
}

#[test]
fn null_clears_only_what_can_be_cleared() {
    let ok = "\
version: 2
project:
  slug: shop
  description: null
";
    parse(ok).expect("description is nullable");

    let environment = "\
version: 2
project:
  slug: shop
  environments:
    staging:
      applications:
        api:
          environment: null
";
    parse(environment).expect("a keyed collection can be cleared as a whole");

    let replicas = "\
version: 2
project:
  slug: shop
  environments:
    staging:
      applications:
        api:
          replicas: null
";
    assert_eq!(
        codes(parse(replicas)),
        [(
            DiagnosticCode::Null,
            "project.environments.staging.applications.api.replicas".into()
        )]
    );
}

// ---------------------------------------------------------------------------
// Structure
// ---------------------------------------------------------------------------

#[test]
fn the_version_must_be_two() {
    for (source, fragment) in [
        ("settings: {}\n", "missing `version: 2`"),
        ("version: 1\nsettings: {}\n", "re-import"),
        ("version: 3\nsettings: {}\n", "must be 2"),
        ("version: two\nsettings: {}\n", "must be 2"),
    ] {
        let error = parse(source).expect_err("invalid version");
        let first = error.iter().next().unwrap();
        assert_eq!(first.code, DiagnosticCode::Version, "{source}");
        assert!(
            first.message.contains(fragment),
            "{source}: {}",
            first.message
        );
    }
}

#[test]
fn a_document_has_exactly_one_known_root() {
    assert_eq!(
        codes(parse("version: 2\nproject: {slug: a}\nsettings: {}\n")),
        [(DiagnosticCode::Root, "settings".into())]
    );
    assert_eq!(
        codes(parse("version: 2\n")),
        [(DiagnosticCode::Root, String::new())]
    );
    assert_eq!(
        codes(parse("- not\n- a mapping\n")),
        [(DiagnosticCode::Root, String::new())]
    );
    assert_eq!(
        codes(parse("version: 2\nsettings: {}\nmoves: []\n")),
        [(DiagnosticCode::UnknownField, "moves".into())]
    );
    assert_eq!(
        codes(parse("version: 2\nproject:\n  name: shop\n")),
        [(DiagnosticCode::Key, "project.slug".into())]
    );
}

#[test]
fn keys_and_names_are_validated() {
    let bad_key = "version: 2\nsettings:\n  registries:\n    Bad_Key:\n      url: x\n";
    assert_eq!(
        codes(parse(bad_key)),
        [(DiagnosticCode::Key, "settings.registries.Bad_Key".into())]
    );
    let bad_slug = "version: 2\nproject:\n  slug: Not Valid\n";
    assert_eq!(
        codes(parse(bad_slug)),
        [(DiagnosticCode::Key, "project.slug".into())]
    );
    let bad_env = "\
version: 2
project:
  slug: shop
  environments:
    staging:
      applications:
        api:
          environment:
            1BAD: x
";
    assert_eq!(codes(parse(bad_env)).len(), 1);
}

#[test]
fn unknown_fields_are_rejected_everywhere_and_the_valid_ones_are_listed() {
    let source = "\
version: 2
settings:
  registries:
    ghcr:
      urll: ghcr.io
  gadgets: {}
";
    let error = parse(source).expect_err("unknown names");
    let all: Vec<_> = error.iter().collect();
    assert_eq!(all.len(), 2);
    assert_eq!(
        (all[0].code, all[0].path.as_str()),
        (
            DiagnosticCode::UnknownField,
            "settings.registries.ghcr.urll"
        )
    );
    assert!(all[0].message.contains("url"), "{}", all[0].message);
    assert_eq!(all[1].path, "settings.gadgets");
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

#[test]
fn values_must_have_the_type_and_obey_the_rules_of_their_field() {
    let wrong = |field: &str, value: &str| {
        let source = format!(
            "version: 2\nproject:\n  slug: shop\n  environments:\n    staging:\n      applications:\n        api:\n          {field}: {value}\n"
        );
        codes(parse(&source))
    };
    let replicas = "project.environments.staging.applications.api.replicas";
    assert_eq!(
        wrong("replicas", "two"),
        [(DiagnosticCode::Type, replicas.into())]
    );
    assert_eq!(
        wrong("replicas", "1.5"),
        [(DiagnosticCode::Type, replicas.into())]
    );
    assert_eq!(
        wrong("replicas", "-1"),
        [(DiagnosticCode::Value, replicas.into())]
    );
    let description = "project.environments.staging.applications.api.description";
    assert_eq!(
        wrong("description", "123"),
        [(DiagnosticCode::Type, description.into())]
    );
    assert_eq!(
        wrong("description", "[a]"),
        [(DiagnosticCode::Type, description.into())]
    );
    let args = "project.environments.staging.applications.api.args";
    assert_eq!(wrong("args", "text"), [(DiagnosticCode::Type, args.into())]);
    assert_eq!(
        wrong("args", "[a, 7]"),
        [(DiagnosticCode::Type, format!("{args}[1]"))]
    );
    let source_path = "project.environments.staging.applications.api.source";
    assert_eq!(
        wrong("source", "{type: svn}"),
        [(DiagnosticCode::Value, format!("{source_path}.type"))]
    );
    assert_eq!(
        wrong("source", "{type: docker, nope: 1}"),
        [(DiagnosticCode::UnknownField, format!("{source_path}.nope"))]
    );
    assert_eq!(
        wrong("source", "{image: nginx}"),
        [(DiagnosticCode::Type, source_path.into())],
        "the tag is required"
    );
}

#[test]
fn a_secret_is_always_a_source_and_its_literal_never_appears_in_a_message() {
    let canary = "hunter2-canary-value";
    let literal =
        format!("version: 2\nsettings:\n  registries:\n    ghcr:\n      password: {canary}\n");
    let error = parse(&literal).expect_err("a literal secret");
    assert_eq!(
        error.iter().next().unwrap().code,
        DiagnosticCode::SourceRequired
    );
    assert!(!error.to_string().contains(canary), "{error}");

    for (value, code) in [
        ("{ env: 1BAD }", DiagnosticCode::Source),
        ("{ file: ../escape }", DiagnosticCode::Source),
        ("{ file: /abs }", DiagnosticCode::Source),
        ("{ env: A, file: b }", DiagnosticCode::Source),
        ("{ nope: x }", DiagnosticCode::Source),
        ("[x]", DiagnosticCode::SourceRequired),
        ("{ vault: { provider: p } }", DiagnosticCode::Source),
    ] {
        let source =
            format!("version: 2\nsettings:\n  registries:\n    ghcr:\n      password: {value}\n");
        assert_eq!(
            codes(parse(&source)),
            [(code, "settings.registries.ghcr.password".to_owned())],
            "{value}"
        );
    }
    let ok = "version: 2\nsettings:\n  registries:\n    ghcr:\n      password: { file: secrets/ghcr.token }\n";
    parse(ok).expect("a relative file source");
}

#[test]
fn every_field_type_is_checked() {
    let source = "\
version: 2
settings:
  widgets:
    w:
      mode: fast
      count: 3
      ratio: 0.5
      enabled: true
      tags: [b, a]
      ports: [80, 443]
      labels: { team: core, tier: web }
      limits: { cpu: 2, note: null }
      token: { env: WIDGET_TOKEN }
      config: { file: files/widget.conf }
      owner: { local: true }
      peer: { name: ghcr }
      link: widget.other
      extra: { any: [1, 2.5, null, { deep: true }] }
      env: { A: x }
      code: ABC
      source: { type: git, url: git@example.com, branch: null }
";
    let document = parse_widget(source).expect("valid");
    let Root::Settings(sections) = &document.root else {
        panic!("settings");
    };
    let w = &sections["widgets"]["w"];
    assert_eq!(
        w.fields["tags"].value,
        Value::List(vec![Value::Text("a".into()), Value::Text("b".into())]),
        "a set is stored in canonical order"
    );
    assert_eq!(w.fields["ratio"].value, Value::Number(0.5));
    assert!(matches!(w.fields["extra"].value, Value::Blob(_)));
    assert_eq!(w.fields["owner"].value, Value::Selector(Selector::Local));
}

#[test]
fn rules_and_shapes_that_only_the_widget_exercises() {
    let wrong = |line: &str| {
        codes(parse_widget(&format!(
            "version: 2\nsettings:\n  widgets:\n    w:\n      {line}\n"
        )))
    };
    let at = |field: &str| format!("settings.widgets.w.{field}");
    assert_eq!(
        wrong("tags: [a, a]"),
        [(DiagnosticCode::Duplicate, at("tags[1]"))]
    );
    assert_eq!(wrong("count: 11"), [(DiagnosticCode::Value, at("count"))]);
    assert_eq!(wrong("ratio: 1.5"), [(DiagnosticCode::Value, at("ratio"))]);
    // A non-finite float is refused by the YAML layer itself.
    assert_eq!(
        wrong("ratio: .nan"),
        [(DiagnosticCode::Syntax, String::new())]
    );
    assert_eq!(wrong("code: abc"), [(DiagnosticCode::Value, at("code"))]);
    assert_eq!(wrong("mode: warp"), [(DiagnosticCode::Value, at("mode"))]);
    assert_eq!(
        wrong("limits: { cpu: 0 }"),
        [(DiagnosticCode::Value, at("limits.cpu"))]
    );
    assert_eq!(
        wrong("limits: { gone: 1 }"),
        [(DiagnosticCode::UnknownField, at("limits.gone"))]
    );
    assert_eq!(
        wrong("owner: { local: false }"),
        [(DiagnosticCode::Value, at("owner"))]
    );
    assert_eq!(
        wrong("peer: { local: true }"),
        [(DiagnosticCode::Value, at("peer"))]
    );
    assert_eq!(
        wrong("owner: { name: ' padded' }"),
        [(DiagnosticCode::Value, at("owner"))]
    );
    assert_eq!(wrong("owner: edge"), [(DiagnosticCode::Type, at("owner"))]);
    assert_eq!(
        wrong("config: inline text"),
        [(DiagnosticCode::SourceRequired, at("config"))]
    );
    assert_eq!(wrong("name: ''"), [(DiagnosticCode::Value, at("name"))]);
    assert_eq!(
        wrong("source: { type: image, image: nginx, password: hunter2 }"),
        [(DiagnosticCode::SourceRequired, at("source.password"))]
    );
    assert_eq!(
        wrong("env: { A: 7 }"),
        [(DiagnosticCode::Type, at("env.A"))]
    );
    assert_eq!(
        wrong("env: { A: { nope: 1 } }"),
        [(DiagnosticCode::Type, at("env.A"))]
    );
}

// ---------------------------------------------------------------------------
// Directives
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_and_depends_on_are_validated() {
    let wrong = |block: &str| {
        codes(parse(&format!(
            "version: 2\nsettings:\n  registries:\n    ghcr:\n{block}"
        )))
    };
    let at = |field: &str| format!("settings.registries.ghcr.{field}");
    assert_eq!(
        wrong("      lifecycle: { protect: maybe }\n"),
        [(DiagnosticCode::Directive, at("lifecycle.protect"))]
    );
    assert_eq!(
        wrong("      lifecycle: { ignore_changes: [nope] }\n"),
        [(DiagnosticCode::Directive, at("lifecycle.ignore_changes[0]"))]
    );
    assert_eq!(
        wrong("      lifecycle: { deploy: x }\n"),
        [(DiagnosticCode::UnknownField, at("lifecycle.deploy"))]
    );
    assert_eq!(
        wrong("      depends_on: registry.main\n"),
        [(DiagnosticCode::Directive, at("depends_on"))]
    );
    assert_eq!(
        wrong("      depends_on: ['']\n"),
        [(DiagnosticCode::Directive, at("depends_on[0]"))]
    );
}

// ---------------------------------------------------------------------------
// Syntax, budgets, and diagnostics
// ---------------------------------------------------------------------------

#[test]
fn unsafe_or_malformed_yaml_is_a_syntax_error() {
    for source in [
        "version: 2\nsettings: [unclosed\n",
        "version: 2\nversion: 2\nsettings: {}\n",
        "version: 2\nsettings: &a {}\nother: *a\n",
        "version: 2\nsettings: !tag {}\n",
        "version: 2\nsettings: {}\n---\nversion: 2\nsettings: {}\n",
        "base: &b {x: 1}\nversion: 2\nsettings:\n  <<: *b\n",
    ] {
        let error = parse(source).expect_err("rejected");
        assert_eq!(
            error.iter().next().unwrap().code,
            DiagnosticCode::Syntax,
            "{source}"
        );
    }
    let huge = format!(
        "version: 2\nsettings: {{}}\n# {}\n",
        "x".repeat(3 * 1024 * 1024)
    );
    assert_eq!(
        parse(&huge)
            .expect_err("too large")
            .iter()
            .next()
            .unwrap()
            .code,
        DiagnosticCode::Syntax
    );
}

#[test]
fn every_problem_is_reported_in_source_order_with_its_position() {
    let source = "\
version: 2
settings:
  registries:
    ghcr:
      type: nope
      url: 7
      sneaky: 1
    other:
      username: ''
";
    let error = parse(source).expect_err("several problems");
    let found: Vec<(u32, u32, DiagnosticCode)> = error
        .iter()
        .map(|d| (d.span.line, d.span.column, d.code))
        .collect();
    assert_eq!(
        found,
        [
            (5, 13, DiagnosticCode::Value),
            (6, 12, DiagnosticCode::Type),
            (7, 7, DiagnosticCode::UnknownField),
            (9, 17, DiagnosticCode::Value),
        ]
    );
    let rendered = error.to_string();
    assert!(rendered.contains("DOKDOC006"), "{rendered}");
    assert!(rendered.lines().count() == 4);
}

#[test]
fn empty_text_breaks_a_length_rule_so_nothing_is_ever_sent() {
    let tag = "\
version: 2
settings:
  tags:
    blue:
      name: \"\"
";
    assert_eq!(
        codes(parse(tag)),
        [(DiagnosticCode::Value, "settings.tags.blue.name".into())]
    );

    let redirect = "\
version: 2
project:
  slug: shop
  environments:
    staging:
      applications:
        api:
          redirects:
            www:
              regex: \"\"
              replacement: /new
              permanent: true
";
    assert_eq!(
        codes(parse(redirect)),
        [(
            DiagnosticCode::Value,
            "project.environments.staging.applications.api.redirects.www.regex".into()
        )]
    );
}
