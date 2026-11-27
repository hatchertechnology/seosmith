//! Thor manifests fail closed: the sender must be pinned to a real image
//! digest before anything under `deploy/thor` can be synced, and the policy
//! `ConfigMap` source must stay byte-identical to the production fixture.

use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn thor_files() -> Vec<(PathBuf, String)> {
    let dir = root().join("deploy/thor");
    let mut out: Vec<_> = fs::read_dir(&dir)
        .expect("deploy/thor exists")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.is_file())
        .map(|p| {
            let text = fs::read_to_string(&p).expect("deploy/thor files are UTF-8");
            (p, text)
        })
        .collect();
    out.sort();
    out
}

fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Lines of `resources.yaml` whose key is `image:` (comments excluded).
fn image_lines() -> Vec<String> {
    let text = fs::read_to_string(root().join("deploy/thor/resources.yaml")).unwrap();
    text.lines()
        .filter(|l| l.trim_start().starts_with("image:"))
        .map(str::to_owned)
        .collect()
}

fn pinned(line: &str, prefix: &str) -> bool {
    line.trim_start()
        .strip_prefix(prefix)
        .is_some_and(is_digest)
}

#[test]
fn no_placeholder_anywhere_under_deploy_thor() {
    for (path, text) in thor_files() {
        assert!(
            !text.contains("PLACEHOLDER"),
            "{} still carries a placeholder; fill the real digest before this can sync",
            path.display()
        );
    }
}

#[test]
fn sender_image_is_one_full_sha256_digest() {
    let lines = image_lines();
    let sender: Vec<_> = lines
        .iter()
        .filter(|l| l.contains("localhost/seosmith"))
        .collect();
    assert_eq!(sender.len(), 1, "exactly one sender image line: {lines:?}");
    assert!(
        pinned(sender[0], "image: localhost/seosmith@sha256:"),
        "sender image must be `image: localhost/seosmith@sha256:<64 hex>`: {:?}",
        sender[0]
    );
}

#[test]
fn init_gate_image_is_one_full_sha256_digest() {
    let lines = image_lines();
    let gate: Vec<_> = lines
        .iter()
        .filter(|l| l.contains("curlimages/curl"))
        .collect();
    assert_eq!(gate.len(), 1, "exactly one init-gate image line: {lines:?}");
    assert!(
        pinned(gate[0], "image: docker.io/curlimages/curl@sha256:"),
        "init-gate image must be pinned by a 64-hex digest: {:?}",
        gate[0]
    );
}

#[test]
fn every_image_line_is_one_of_the_two_pinned_images() {
    let lines = image_lines();
    assert_eq!(lines.len(), 2, "unexpected image lines: {lines:?}");
}

#[test]
fn policy_copy_is_byte_identical_to_the_fixture() {
    let kustomization = fs::read_to_string(root().join("deploy/thor/kustomization.yaml")).unwrap();
    if !kustomization.contains("policy.json") {
        // 4a mounts no advisory policy, so there is no copy to keep honest.
        return;
    }
    let copy = fs::read(root().join("deploy/thor/policy.json"))
        .expect("policy.json beside the kustomization");
    let fixture = fs::read(root().join("fixtures/policies/production-default.json")).unwrap();
    assert_eq!(
        copy, fixture,
        "deploy/thor/policy.json drifted from the fixture"
    );
}

// ---------------------------------------------------------------- wire-2 flags

/// `(name, value)` of every `- name: X` / `value: "Y"` pair under `env:` in
/// `resources.yaml`. A `value:` on the line after the name; `valueFrom` entries
/// and comments are not pairs.
fn env_pairs() -> Vec<(String, String)> {
    let text = fs::read_to_string(root().join("deploy/thor/resources.yaml")).unwrap();
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut out = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let Some(name) = line.strip_prefix("- name: ") else {
            continue;
        };
        if let Some(value) = lines.get(n + 1).and_then(|l| l.strip_prefix("value: ")) {
            out.push((name.to_owned(), value.trim_matches('"').to_owned()));
        }
    }
    out
}

fn flag(name: &str) -> Vec<String> {
    env_pairs()
        .into_iter()
        .filter(|(n, _)| n == name)
        .map(|(_, v)| v)
        .collect()
}

#[test]
fn wire2_flag_is_stated_once_and_false_until_the_rollout() {
    assert_eq!(
        flag("SEOSMITH_POLICY_WIRE_V2"),
        ["false"],
        "SEOSMITH_POLICY_WIRE_V2 must be stated exactly once, as \"false\"; the R6 rollout flips it"
    );
}

#[test]
fn flag_combination_is_one_the_binary_starts_with() {
    // The binary refuses: wire-2 without wire-1 or without a policy, wire-1 or
    // wire-2 together with the schema-2 evidence gate, and wire-1 without a policy.
    let get = |n: &str| match flag(n).as_slice() {
        [v] => v.clone(),
        other => panic!("{n} must be stated exactly once: {other:?}"),
    };
    let ev2 = get("SEOSMITH_EVIDENCE_CONTRACT_V2") == "true";
    let w1 = get("SEOSMITH_POLICY_WIRE_V1") == "true";
    let w2 = get("SEOSMITH_POLICY_WIRE_V2") == "true";
    let policy = !flag("SEOSMITH_ADVISORY_POLICY").is_empty();
    for name in [
        "SEOSMITH_EVIDENCE_CONTRACT_V2",
        "SEOSMITH_POLICY_WIRE_V1",
        "SEOSMITH_POLICY_WIRE_V2",
    ] {
        assert!(["true", "false"].contains(&get(name).as_str()), "{name}");
    }
    assert!(!(w1 && ev2), "wire-1 with the schema-2 evidence gate");
    assert!(!(w2 && ev2), "wire-2 with the schema-2 evidence gate");
    assert!(!w2 || w1, "wire-2 needs SEOSMITH_POLICY_WIRE_V1=true");
    assert!(!w2 || policy, "wire-2 needs an advisory policy");
    assert!(!w1 || policy, "wire-1 needs an advisory policy");
}

#[test]
fn readme_spells_the_three_flag_names_in_full_and_the_naming_trap() {
    let readme = fs::read_to_string(root().join("deploy/thor/README.md")).unwrap();
    for name in [
        "SEOSMITH_POLICY_WIRE_V2",
        "SEOSMITH_POLICY_WIRE_V1",
        "SEOSMITH_EVIDENCE_CONTRACT_V2",
    ] {
        assert!(readme.contains(name), "README must spell {name}");
    }
    assert!(
        readme.contains("older **schema-2** evidence gate"),
        "README must say that SEOSMITH_EVIDENCE_CONTRACT_V2 is the schema-2 gate, not wire-2"
    );
    assert!(
        readme.contains("one-word flip"),
        "rollback is the one-word flip"
    );
}

// ------------------------------------------------- the thor `/tmp` emptyDir

#[test]
fn tmp_emptydir_is_mounted_and_bounded_and_the_root_stays_read_only() {
    // Thor wedged on SQLITE_IOERR_GETTEMPPATH without a writable /tmp
    // (coordination/tasks/w1c-thor-repro.md); the manifest change must not regress.
    let text = fs::read_to_string(root().join("deploy/thor/resources.yaml")).unwrap();
    assert!(text.contains("readOnlyRootFilesystem: true"));
    assert!(
        text.contains("- name: tmp\n              mountPath: /tmp"),
        "the seosmith container must mount the `tmp` volume at /tmp"
    );
    assert!(
        text.contains("- name: tmp\n          emptyDir:\n            sizeLimit: 128Mi"),
        "the `tmp` volume must be an emptyDir with a size limit"
    );
}
