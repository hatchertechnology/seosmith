use seosmith::{model::Report, policy::AdvisoryPolicy, policy_wire_prototype};
fn main() -> anyhow::Result<()> {
    let case = std::env::args().nth(1).unwrap_or_default();
    let base = if case == "omitted" {
        include_str!("../fixtures/policies/v3-local-omitted.json")
    } else {
        include_str!("../fixtures/policies/v3-local-complete.json")
    };
    let mut report: Report = serde_json::from_str(base)?;
    report.schema_version = 1;
    report.policy_guidance = None;
    report.findings.clear();
    match case.as_str() {
        "control" => {}
        "ambiguous" => {
            let mut duplicate = report.pages[3].clone();
            duplicate.is_resource = !duplicate.is_resource;
            report.pages.push(duplicate);
            seosmith::policy::apply_with_evidence(&mut report, &AdvisoryPolicy::default())?;
            print!("{}", serde_json::to_string(&report)?);
            return Ok(());
        }
        "source_missing_status" => report.pages[0].status = None,
        "source_non_html" => report.pages[0].content_type = "application/json".into(),
        "source_non_success" => report.pages[0].status = Some(404),
        "source_error" => report.pages[0].error = Some("fetch_failed".into()),
        "source_blocked" => report.pages[0].blocked_by_robots = true,
        "source_truncated" => report.pages[0].extraction_truncated = true,
        "target_missing_status" => report.pages[3].status = None,
        "target_error" => report.pages[3].error = Some("fetch_failed".into()),
        "target_blocked" => report.pages[3].blocked_by_robots = true,
        "omitted" => {
            for (i, page) in report.pages.iter_mut().enumerate().skip(4) {
                if i % 3 == 0 {
                    page.extraction_truncated = false;
                    page.status = None;
                }
                if i % 3 == 1 {
                    page.extraction_truncated = false;
                }
            }
        }
        _ => anyhow::bail!("unknown case"),
    }
    let bytes = policy_wire_prototype::generate(&mut report, &AdvisoryPolicy::default())?;
    print!("{}", String::from_utf8(bytes)?);
    Ok(())
}
