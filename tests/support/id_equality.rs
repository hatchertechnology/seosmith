//! Shared ID-equality check (plan §5 E-p, S1 review MINOR-4): every `fail`
//! evaluation's `finding_key` must be a delivered finding's `key`, resolved in
//! the **serialised delivered bytes**. Used by the S2 fixture test over every
//! committed schema-4 fixture, including the crawl fixture.
#![allow(dead_code)]

use seosmith::{audit, contract};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub struct Equality {
    /// Findings of `audit::rules()` rules dropped by the observation or
    /// evaluation caps (excluded on both sides).
    pub excluded: usize,
    pub fails: usize,
    pub evaluations: usize,
    pub truncated: bool,
}

pub fn assert_id_equality(bytes: &[u8]) -> Equality {
    let doc: Value = serde_json::from_slice(bytes).expect("fixture parses");
    let findings = doc["findings"].as_array().expect("findings");
    let pages = doc["pages"].as_array().expect("pages");
    let evidence = &doc["evidence_contract"];
    let evals = evidence["evaluations"].as_array().expect("evaluations");
    let instrumented: HashSet<&str> = ["missing_title"].into();
    let rules: Vec<&str> = audit::rules().into_iter().map(|rule| rule.id).collect();

    let mut k_ev: Vec<String> = Vec::new();
    for e in evals {
        let outcome = e["outcome"].as_str().expect("outcome");
        if outcome == "fail" {
            let key = e["finding_key"].as_str().expect("fail needs finding_key");
            let locator = e["source_locator"].as_str().expect("fail needs a locator");
            let n: usize = locator
                .strip_prefix("/findings/")
                .expect("fail points at a finding")
                .parse()
                .expect("finding index");
            assert_eq!(findings[n]["key"], key, "{locator} key");
            assert_eq!(findings[n]["rule"], e["rule"], "{locator} rule");
            k_ev.push(key.to_owned());
        } else {
            assert!(
                e["finding_key"].is_null(),
                "only fail carries a finding_key"
            );
        }
        if outcome == "pass" {
            assert!(
                instrumented.contains(e["rule"].as_str().unwrap()),
                "pass for an uninstrumented rule"
            );
        }
    }

    // K_del: delivered findings whose (rule, URL) received an evaluation slot,
    // simulating the MAX_OBSERVATIONS / MAX_EVALUATIONS order independently.
    let mut by_rule_url: HashMap<(&str, &str), &str> = HashMap::new();
    for f in findings {
        let rule = f["rule"].as_str().unwrap();
        if rules.contains(&rule) {
            let url = f["url"].as_str().unwrap();
            let previous = by_rule_url.insert((rule, url), f["key"].as_str().unwrap());
            assert!(previous.is_none(), "duplicate delivered ({rule}, {url})");
        }
    }
    let (mut k_del, mut excluded, mut slots) = (Vec::new(), 0, 0);
    for rule in &rules {
        for (i, page) in pages.iter().enumerate() {
            let found = by_rule_url.get(&(*rule, page["url"].as_str().unwrap()));
            if i >= contract::MAX_OBSERVATIONS || slots >= contract::MAX_EVALUATIONS {
                excluded += usize::from(found.is_some());
            } else if found.is_some() || instrumented.contains(rule) {
                slots += 1;
                if let Some(key) = found {
                    k_del.push((*key).to_owned());
                }
            }
        }
    }
    assert_eq!(evals.len(), slots, "evaluation slot count");
    let fails = k_ev.len();
    k_ev.sort();
    k_del.sort();
    assert_eq!(k_ev, k_del, "K_ev must equal K_del as multisets");
    let truncated = evidence["truncated"].as_bool().expect("truncated");
    if excluded > 0 {
        assert!(truncated, "findings past the caps require truncated = true");
    }
    Equality {
        excluded,
        fails,
        evaluations: evals.len(),
        truncated,
    }
}
