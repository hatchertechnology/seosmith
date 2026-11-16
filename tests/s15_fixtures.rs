use seosmith::{audit, fetch::Response, parse};
use std::{collections::BTreeMap, fs, path::Path};
use url::Url;

#[test]
fn labeled_metadata_fixtures_match_parser_and_audit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/s15");
    let labels: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("labels.json")).unwrap()).unwrap();
    for case in labels["cases"].as_array().unwrap() {
        let Some(html) = case["html"].as_str() else {
            continue;
        };
        let url = Url::parse(case["url"].as_str().unwrap()).unwrap();
        let rule = case["rule"].as_str().unwrap();
        let expected = case["expected"].as_bool().unwrap();
        let response = Response {
            status: 200,
            headers: BTreeMap::from([("content-type".into(), "text/html".into())]),
            body: fs::read(root.join(html)).unwrap(),
            ttfb_ms: 0,
        };
        let page = parse::parse(&url, 0, false, &response);
        assert_eq!(
            audit::analyze(&[page], &url).iter().any(|f| f.rule == rule),
            expected,
            "{url}: {rule}"
        );
    }
}
