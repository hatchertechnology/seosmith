use seosmith::{
    model::{Link, Report, normalize},
    policy::AdvisoryPolicy,
    policy_wire_prototype,
};
fn main() -> anyhow::Result<()> {
    let mut r: Report =
        serde_json::from_str(include_str!("../fixtures/policies/v3-local-complete.json"))?;
    r.schema_version = 1;
    r.policy_guidance = None;
    r.findings.clear();
    r.pages[0].links.clear();
    let raw = [
        "https://EXAMPLE.COM/path",
        "https://%65xample.com/path",
        "https://example.com:443/path",
        "https://example.com:8443/path",
        "https://outside.example:8443/path",
        "http://[2001:db8::1]/path",
        "http://[::ffff:192.0.2.1]/path",
        "http://127.1/path",
        "https://bücher.example/path",
        "https://user@example.com/path",
        "https://example.com\\@outside.example/path",
        "https://[bad/path",
    ];
    let mut cases = Vec::new();
    for input in raw {
        let normalized = normalize(&r.request.url, input);
        cases.push(serde_json::json!({"input":input,"normalized":normalized.as_ref().map(url::Url::as_str),"in_scope":normalized.as_ref().map(|u| r.request.in_scope(u))}));
        if let Some(url) = normalized {
            r.pages[0].links.push(Link {
                url: url.to_string(),
                ..Link::default()
            });
        }
    }
    if std::env::args().nth(1).as_deref() == Some("matrix") {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"status":"candidate-not-frozen","scope":"Local producer normalization analysis only; not a wire URL contract.","cases":cases})
            )?
        );
    } else {
        let bytes = policy_wire_prototype::generate(&mut r, &AdvisoryPolicy::default())?;
        print!("{}", String::from_utf8(bytes)?);
    }
    Ok(())
}
