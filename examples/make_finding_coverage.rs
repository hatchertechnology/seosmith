use seosmith::{finding_coverage_prototype, policy::AdvisoryPolicy};

fn main() -> anyhow::Result<()> {
    let dir = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("output directory required"))?;
    std::fs::create_dir_all(&dir)?;
    for case in [
        "complete-missing-h1",
        "complete-clean-h1",
        "partial-missing-h1",
        "partial-clean-h1",
    ] {
        let mut report = finding_coverage_prototype::fixture_input(case)?;
        let bytes = finding_coverage_prototype::generate(&mut report, &AdvisoryPolicy::default())?;
        std::fs::write(format!("{dir}/{case}.json"), bytes)?;
    }
    Ok(())
}
