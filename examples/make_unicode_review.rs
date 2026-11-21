use seosmith::{
    model::Report,
    policy::{AdvisoryPolicy, apply_with_evidence},
};
fn main() -> anyhow::Result<()> {
    let mut r: Report =
        serde_json::from_str(include_str!("../fixtures/policies/v3-local-complete.json"))?;
    let case = std::env::args().nth(1).unwrap_or_default();
    r.pages[0].title = match case.as_str() {
        "short" => "😀😀",
        "equal" => "😀😀a",
        _ => anyhow::bail!("unknown case"),
    }
    .into();
    r.findings.clear();
    r.policy_guidance = None;
    let policy = AdvisoryPolicy {
        title_min: 3,
        ..AdvisoryPolicy::default()
    };
    apply_with_evidence(&mut r, &policy)?;
    print!("{}", serde_json::to_string(&r)?);
    Ok(())
}
