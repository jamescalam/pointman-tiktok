//! extension.toml within what core's manifest check takes (Pointman's src/messageboard/extensions/manifest.py), and
//! its schemas as proto/tiktok/v1/tiktok.proto says them (the SDK's `proto::check`).

use regex::Regex;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn manifest() -> toml::Table {
    toml::from_str(&std::fs::read_to_string(format!("{ROOT}/extension.toml")).unwrap()).unwrap()
}

#[test]
fn the_manifest_keeps_to_cores_limits() {
    let m = manifest();
    let chars = |v: &toml::Value| v.as_str().unwrap().chars().count();
    let ext = &m["extension"];
    assert_eq!(ext["version"].as_str(), Some(env!("CARGO_PKG_VERSION")), "extension.toml's version is Cargo's");
    assert!((1..=60).contains(&chars(&ext["name"])) && (1..=200).contains(&chars(&ext["summary"])));
    assert_eq!(m["run"]["command"].as_array().unwrap()[0].as_str(), Some("bin/tiktok"), "a binary runs as bin/<id>");
    assert!(std::path::Path::new(ROOT).join("SKILL.md").is_file(), "SKILL.md ships with the behaviour");
    let slash = Regex::new("^[a-z][a-z0-9-]{0,23}$").unwrap();
    for c in m["commands"].as_array().unwrap() {
        let kind = c["kind"].as_str().unwrap();
        assert!(kind.starts_with("tiktok."), "{kind}");
        assert!(chars(&c["title"]) <= 60, "{kind}'s title is over 60");
        assert!(chars(&c["summary"]) <= 120, "{kind}'s summary is over 120");
        assert!(slash.is_match(c["slash"].as_str().unwrap()), "{kind}'s slash");
    }
    let forms: Vec<&str> = m["credentials"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(forms, ["client", "account"]);
    for c in m["credentials"].as_array().unwrap() {
        let value = Regex::new(&format!("^(?:{})$", c["value"].as_str().unwrap())).unwrap();
        let example = match c["id"].as_str().unwrap() {
            "client" => "awfakeclientkey01 fakeclientsecret0123456789abcdef",
            _ => "rft.example12345Example12345Example",
        };
        assert!(value.is_match(example), "{} takes {example}", c["id"]);
    }
}

/// The schemas are written from the proto by `pointman-extension manifest write`: this fails while they say something
/// else (the README says how to install it).
#[test]
fn the_schemas_are_the_protos() {
    if let Err(e) = pointman_extension::proto::check(ROOT.as_ref()) {
        panic!("{e:#}");
    }
}
