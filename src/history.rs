//! Checks against the specification of every released VCam API version in `spec/history/`,
//! since people keep using the VCam version they bought.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::mcp::tools::tool_definitions;
use crate::spec::{Method, Spec};

fn version_key(version: &str) -> (u64, u64, u64) {
    let mut parts = version.split('.').map(|p| {
        p.parse::<u64>()
            .unwrap_or_else(|_| panic!("'{version}' is not a version"))
    });
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// Released specifications by version, oldest first
fn released() -> Vec<Spec> {
    let directory = concat!(env!("CARGO_MANIFEST_DIR"), "/spec/history");
    let mut specs = BTreeMap::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let document: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let spec = Spec::new(document);
        let file_version = path.file_stem().unwrap().to_str().unwrap().to_owned();
        assert_eq!(
            spec.version(),
            file_version,
            "{} is named after another version",
            path.display()
        );
        specs.insert(version_key(&file_version), spec);
    }
    assert!(!specs.is_empty());
    specs.into_values().collect()
}

#[test]
fn builds_tools_for_every_released_version() {
    for spec in released() {
        for tool in tool_definitions(&spec, &[]) {
            let schema = tool.input_schema.to_string();
            for reference in schema.split("\"$ref\":\"").skip(1) {
                let name = reference
                    .split('"')
                    .next()
                    .unwrap()
                    .strip_prefix("#/$defs/")
                    .unwrap_or_else(|| {
                        panic!(
                            "{} of {} points outside the tool",
                            tool.name,
                            spec.version()
                        )
                    });
                assert!(
                    tool.input_schema["$defs"].get(name).is_some(),
                    "{} of {} misses {name}",
                    tool.name,
                    spec.version()
                );
            }
        }
        assert!(spec.method("rpc.discover").is_some());
        assert!(spec.method("events.subscribe").is_some());
    }
}

/// Every method, parameter, and event of an older version stays as it was, and what a newer
/// version adds is marked with the version that added it.
#[test]
fn every_version_keeps_what_older_ones_offer() {
    let mut specs = released();
    let bundled = Spec::bundled();
    let latest = specs.last().unwrap().version().to_owned();
    assert!(
        version_key(bundled.version()) >= version_key(&latest),
        "spec/openrpc.json is older than the released {latest}"
    );
    specs.push(bundled);
    for pair in specs.windows(2) {
        let (older, newer) = (&pair[0], &pair[1]);
        if older.version() == newer.version() {
            assert_eq!(
                names(older),
                names(newer),
                "the API changed without a new version after {}",
                older.version()
            );
        }
        for old in older.methods().chain(older.events()) {
            let new = newer
                .methods()
                .chain(newer.events())
                .find(|m| m.name() == old.name())
                .unwrap_or_else(|| panic!("{} removed {}", newer.version(), old.name()));
            for param in old.params() {
                let kept = new.param(param.name).unwrap_or_else(|| {
                    panic!(
                        "{} removed --{} of {}",
                        newer.version(),
                        param.name,
                        old.name()
                    )
                });
                assert!(
                    param.required || !kept.required,
                    "{} made --{} of {} required",
                    newer.version(),
                    param.name,
                    old.name()
                );
            }
        }
        for new in newer.methods().chain(newer.events()) {
            match older
                .methods()
                .chain(older.events())
                .find(|m| m.name() == new.name())
            {
                None => assert_added(older, newer, new.name(), new.since()),
                Some(old) => {
                    for param in new
                        .params()
                        .into_iter()
                        .filter(|p| old.param(p.name).is_none())
                    {
                        assert_added(
                            older,
                            newer,
                            &format!("--{} of {}", param.name, new.name()),
                            param.since,
                        );
                        assert!(
                            !param.required,
                            "{} added a required --{} to {}",
                            newer.version(),
                            param.name,
                            new.name()
                        );
                    }
                }
            }
        }
    }
}

fn assert_added(older: &Spec, newer: &Spec, what: &str, since: Option<&str>) {
    let since = since.unwrap_or_else(|| {
        panic!(
            "{what}, added after {}, has no x-vcam-since",
            older.version()
        )
    });
    let since = version_key(since);
    assert!(
        version_key(older.version()) < since && since <= version_key(newer.version()),
        "{what} has x-vcam-since outside {}..={}",
        older.version(),
        newer.version()
    );
}

fn names(spec: &Spec) -> Vec<(String, Vec<String>)> {
    let describe = |m: Method<'_>| {
        (
            m.name().to_owned(),
            m.params().iter().map(|p| p.name.to_owned()).collect(),
        )
    };
    spec.methods().chain(spec.events()).map(describe).collect()
}
