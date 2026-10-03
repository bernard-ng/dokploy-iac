use dokploy_spec::{VersionStatus, Versions};

const DIGEST: &str = "1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8";

fn entry(version: &str, status: &str, openapi: bool) -> String {
    let openapi = if openapi {
        "    openapi: openapi/dokploy.json\n"
    } else {
        ""
    };
    format!(
        "  - version: \"{version}\"\n    status: {status}\n    image: dokploy/dokploy:v{version}@sha256:{DIGEST}\n{openapi}"
    )
}

fn parse(entries: &[String]) -> Result<Versions, String> {
    Versions::parse(&format!("versions:\n{}", entries.concat()))
}

#[test]
fn the_repository_file_is_valid() {
    let specs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
    let versions = Versions::load(&specs).expect("specs/versions.yaml is valid");
    let default = versions.default_version().expect("a supported version");
    assert_eq!(default.status, VersionStatus::Supported);
}

#[test]
fn lookup_accepts_both_spellings_and_the_default_is_the_first_supported() {
    let versions = parse(&[
        entry("0.30.5", "candidate", false),
        entry("0.30.6", "supported", true),
        entry("0.30.7", "supported", true),
    ])
    .unwrap();

    assert_eq!(versions.find("0.30.7").unwrap().tag(), "v0.30.7");
    assert_eq!(versions.find("v0.30.7").unwrap().version, "0.30.7");
    assert!(versions.find("0.30.8").is_none());
    assert_eq!(versions.default_version().unwrap().version, "0.30.6");
    assert_eq!(
        versions.find("0.30.6").unwrap().fixture_directory(),
        "fixtures/api/live/v0.30.6"
    );
}

#[test]
fn an_image_that_is_not_digest_pinned_to_its_own_tag_is_rejected() {
    let unpinned = "  - version: \"0.30.6\"\n    status: supported\n    image: dokploy/dokploy:v0.30.6\n    openapi: openapi/dokploy.json\n".to_owned();
    assert!(parse(&[unpinned]).unwrap_err().contains("image must be"));

    let wrong_tag = entry("0.30.6", "supported", true).replace("v0.30.6@", "v0.30.7@");
    assert!(parse(&[wrong_tag]).unwrap_err().contains("image must be"));

    let short = entry("0.30.6", "supported", true).replace(DIGEST, "abc");
    assert!(parse(&[short]).unwrap_err().contains("image must be"));
}

#[test]
fn structural_mistakes_are_rejected() {
    assert!(
        parse(&[entry("v0.30.6", "supported", true)])
            .unwrap_err()
            .contains("without a leading `v`")
    );
    assert!(
        parse(&[
            entry("0.30.6", "supported", true),
            entry("0.30.6", "candidate", false)
        ])
        .unwrap_err()
        .contains("listed twice")
    );
    assert!(
        parse(&[entry("0.30.6", "candidate", false)])
            .unwrap_err()
            .contains("no version is supported")
    );
    assert!(
        parse(&[entry("0.30.6", "supported", false)])
            .unwrap_err()
            .contains("needs a vendored `openapi`")
    );
    assert!(parse(&[entry("0.30.6", "retired", true)]).is_err());
    assert!(
        Versions::parse("versions: []\nextra: 1\n")
            .unwrap_err()
            .contains("extra")
    );
}

#[test]
fn the_embedded_specs_are_the_specs_on_disk() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
    let on_disk = dokploy_spec::load_dir(&root).expect("specs load");
    let embedded = dokploy_spec::embedded().expect("embedded specs load");

    let names = |registry: &dokploy_spec::SpecRegistry| {
        registry
            .kinds()
            .map(|spec| spec.kind.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&on_disk), names(&embedded));
    for spec in on_disk.kinds() {
        assert_eq!(
            Some(spec),
            embedded.get(&spec.kind),
            "{} differs",
            spec.kind
        );
    }
}
