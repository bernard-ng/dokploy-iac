mod common;

use common::generate::document;
use common::{repository, widget};
use dokploy_model::Document;
use dokploy_spec::Scope;

const CASES: u64 = 400;

fn check(registry: &dokploy_spec::SpecRegistry, scope: Scope, seed: u64) {
    let original = document(seed, registry, scope);
    let text = original.render();
    let parsed = Document::parse(&text, registry).unwrap_or_else(|error| {
        panic!("seed {seed} rendered an invalid document:\n{error}\n---\n{text}")
    });
    if parsed != original {
        let differing: Vec<_> = original
            .resources()
            .into_iter()
            .zip(parsed.resources())
            .flat_map(|(before, after)| {
                before.fields.iter().filter_map(move |(name, field)| {
                    let other = after.fields.get(name).map(|f| &f.value);
                    (other != Some(&field.value)).then(|| {
                        format!("{}.{name}: {:?} became {other:?}", before.key, field.value)
                    })
                })
            })
            .collect();
        panic!("seed {seed} changed meaning: {differing:#?}\n{text}");
    }
    assert_eq!(
        parsed.render(),
        text,
        "seed {seed}: rendering is not a fixed point"
    );
}

#[test]
fn random_settings_documents_round_trip() {
    for seed in 0..CASES {
        check(repository(), Scope::Settings, seed);
    }
}

#[test]
fn random_project_documents_round_trip() {
    for seed in 0..CASES {
        check(repository(), Scope::Project, seed);
    }
}

#[test]
fn documents_that_use_every_field_type_round_trip() {
    for seed in 0..CASES {
        check(widget(), Scope::Settings, seed);
    }
}

#[test]
fn rendering_is_canonical_whatever_the_input_layout() {
    let messy = "\
settings:
  registries:
    ghcr:
      url: \"ghcr.io\"
      username: ci
      type: cloud
      password: { env: TOKEN }
version: 2
";
    let canonical = Document::parse(messy, repository()).unwrap().render();
    assert_eq!(
        canonical,
        "\
version: 2
settings:
  registries:
    ghcr:
      password:
        env: TOKEN
      type: cloud
      url: ghcr.io
      username: ci
"
    );
    // Reordered or re-quoted input renders to the same text.
    assert_eq!(
        Document::parse(&canonical, repository()).unwrap().render(),
        canonical
    );
}

#[test]
fn awkward_text_survives_a_round_trip() {
    let widget = widget();
    for (index, awkward) in common::generate::awkward().iter().enumerate() {
        let source = format!(
            "version: 2\nsettings:\n  widgets:\n    w{index}:\n      link: {}\n",
            serde_json::to_string(awkward).unwrap()
        );
        let parsed = Document::parse(&source, widget).expect("a quoted string parses");
        let text = parsed.render();
        let again = Document::parse(&text, widget).expect("the canonical text parses");
        assert_eq!(again, parsed, "{awkward:?} -> {text}");
    }
}
