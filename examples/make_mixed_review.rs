use seosmith::{
    model::Report,
    policy::{AdvisoryPolicy, apply_with_evidence},
};
fn main() -> anyhow::Result<()> {
    let mut r: Report =
        serde_json::from_str(include_str!("../fixtures/policies/v3-local-omitted.json"))?;
    for p in r.pages.iter_mut().skip(4).step_by(3) {
        p.extraction_truncated = false;
    }
    r.findings.clear();
    r.policy_guidance = None;
    apply_with_evidence(&mut r, &AdvisoryPolicy::default())?;
    print!("{}", serde_json::to_string(&r)?);
    Ok(())
}
