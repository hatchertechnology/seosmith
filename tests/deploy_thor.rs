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
