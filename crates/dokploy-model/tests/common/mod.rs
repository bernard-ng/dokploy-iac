#![allow(dead_code)]

pub mod generate;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

use dokploy_spec::{SpecRegistry, load_dir, parse_spec};

/// The repository's own specs.
pub fn repository() -> &'static SpecRegistry {
    static SPECS: OnceLock<SpecRegistry> = OnceLock::new();
    SPECS.get_or_init(|| {
        load_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs"))
            .expect("repository specs are valid")
    })
}

/// A settings kind that uses every field type, class, and mutability.
const WIDGET: &str = r#"
kind: widget
scope: settings
section: widgets
title: Widget
identity: { key: name, collision: [name], address: "widget.{key}" }
api:
  id: widgetId
  create: { op: widget.create }
  update: { op: widget.update }
  remove: { op: widget.remove }
  read:
    list: { op: widget.all, authority: authoritative }
  create_identity:
    from_response: /widgetId
fields:
  name: { type: text, min_len: 1, default: key }
  mode: { type: "enum[fast, slow]" }
  count: { type: int, min: 0, max: 10 }
  ratio: { type: number, min: 0, max: 1 }
  enabled: { type: bool }
  tags: { type: "set<text>" }
  ports: { type: "list<int>" }
  labels: { type: "map<text, text>", granularity: key }
  limits:
    type: struct
    granularity: field
    members:
      cpu: { type: int, min: 1 }
      note: { type: text, nullable: true }
  token: { type: text, class: secret, mutability: write_only, nullable: true }
  config: { type: file, class: content, nullable: true }
  owner: { type: selector(server), nullable: true }
  peer: { type: selector(registry), nullable: true }
  link: { type: ref(widget), nullable: true }
  extra: { type: blob(extra), nullable: true }
  env: { type: env, granularity: key }
  code: { type: text, pattern: "^[A-Z]{3}$", nullable: true }
  source:
    type: union(type)
    arms:
      git:
        url: { type: text }
        branch: { type: text, nullable: true }
      image:
        image: { type: text }
        password: { type: text, class: secret, mutability: write_only, nullable: true }
write:
  - op: widget.update
    fields: [name, mode, count, ratio, enabled, tags, ports, labels, limits, token, config, owner, peer, link, extra, env, code, source]
    shape: partial
"#;

/// The widget kind alone.
pub fn widget() -> &'static SpecRegistry {
    static SPECS: OnceLock<SpecRegistry> = OnceLock::new();
    SPECS.get_or_init(|| {
        let spec = parse_spec(WIDGET).expect("the widget spec parses");
        let mut shared = BTreeSet::new();
        shared.insert("extra".to_owned());
        SpecRegistry::from_specs(vec![spec], &shared)
            .unwrap_or_else(|issues| panic!("the widget spec is valid: {issues:?}"))
    })
}
