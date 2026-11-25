//! TEMPORARY STUB (S1), NOT PART OF THE FINAL CHANGE.
//!
//! E-p (branch `s14/schema4-evidence-p`) owns `contract::attach_v1_schema4`.
//! Until S1 is rebased onto it, this stub has the same signature and contract so
//! S1 can be built and tested. It delegates to the schema-2 builder and undoes
//! only the version bump. It is NOT the final evaluation semantics (E-p builds
//! evidence over the final delivered findings); tests that depend on those
//! semantics are marked `needs E-p`.
//!
//! On rebase: delete this file, remove `pub mod contract_stub_ep;` from
//! `src/lib.rs`, and change the one `use` line in `policy_wire_prototype.rs` to
//! `crate::contract::attach_v1_schema4`.
use crate::model::Report;
use anyhow::{Result, ensure};

/// Same signature as E-p's `contract::attach_v1_schema4`.
pub fn attach_v1_schema4(report: &mut Report) -> Result<()> {
    ensure!(
        report.schema_version == 1
            && report.policy_guidance.is_some()
            && report.evidence_contract.is_none(),
        "evidence v1 attaches to an unversioned policy report"
    );
    crate::contract::enable_v2(report)?;
    report.schema_version = 1;
    Ok(())
}
