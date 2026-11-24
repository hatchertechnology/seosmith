use seosmith::{
    model::{Page, StructuredData},
    structured_validation::validate,
};
use serde_json::json;

fn block(data: serde_json::Value) -> StructuredData {
    StructuredData {
        format: "json_ld".into(),
        data: Some(data),
        error: None,
    }
}
fn page() -> Page {
    Page {
        url: "https://example.com/a".into(),
        status: Some(200),
        content_type: "text/html".into(),
        ..Page::default()
    }
}

#[test]
fn graph_paths_profiles_and_local_references() {
    let data = block(json!({"@context":"https://schema.org", "@graph":[
        {"@type":"Product", "name":"Widget", "offers":{"@id":"#offer"}},
        {"@id":"#offer", "@type":"Offer"},
        {"@type":"LocalBusiness", "name":42, "address":{"@id":"#missing"}}
    ]}));
    let result = validate(&page(), &[data]);
    assert!(!result.iter().any(|o| o.code == "missing_product_evidence"));
    assert!(
        result
            .iter()
            .any(|o| o.code == "unexpected_value_type" && o.path == "/0/data/@graph/2/name")
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "unresolved_local_id" && o.path == "/0/data/@graph/2/address/@id")
    );
    assert!(
        !result
            .iter()
            .any(|o| o.code == "unresolved_local_id" && o.path.contains("/0/offers"))
    );
    assert!(result.iter().any(|o| o.category == "human_review"));
}

#[test]
fn syntax_microdata_and_partial_coverage() {
    let bad = StructuredData {
        format: "json_ld".into(),
        data: None,
        error: Some("line 1 column 2".into()),
    };
    let micro = StructuredData {
        format: "microdata".into(),
        data: Some(json!({"types":["https://schema.org/BreadcrumbList"], "properties":{}})),
        error: None,
    };
    let p = Page {
        extraction_truncated: true,
        ..page()
    };
    let result = validate(&p, &[bad, micro]);
    assert!(
        result
            .iter()
            .any(|o| o.category == "syntax" && o.path == "/0/error")
    );
    assert!(result.iter().any(|o| o.code == "missing_required" && o.path == "/1/data/properties/itemListElement"));
    assert!(result.iter().any(|o| o.category == "coverage"));
}

#[test]
fn resource_status_preserves_unknowns() {
    use seosmith::structured_validation::referenced_resource;
    let resource = Page {
        url: "https://example.com/image.png".into(),
        status: Some(404),
        ..Page::default()
    };
    assert_eq!(
        referenced_resource("https://example.com/a", "/image.png", &[resource]),
        ("observed", Some(404))
    );
    assert_eq!(
        referenced_resource("https://example.com/a", "/missing", &[]),
        ("not_observed", None)
    );
}

#[test]
fn nested_breadcrumb_and_resource_evidence() {
    use seosmith::structured_validation::validate_crawl;
    let p = Page {
        structured_data: vec![block(
            json!({"@type":"BreadcrumbList", "itemListElement":[{"@type":"ListItem","name":"Home"}], "image":"/img.png"}),
        )],
        ..page()
    };
    let result = validate_crawl(&p, &[]);
    assert!(
        result
            .iter()
            .any(|o| o.code == "missing_breadcrumb_property"
                && o.path == "/0/data/itemListElement/0/position")
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "referenced_resource_status"
                && o.explanation.contains("not_observed"))
    );
}

#[test]
fn output_limit_is_explicit_and_resource_fragments_are_ignored() {
    use seosmith::structured_validation::referenced_resource;
    let items: Vec<_> = (0..600).map(|_| json!({"@type":"Product"})).collect();
    let result = validate(&page(), &[block(json!(items))]);
    assert_eq!(result.len(), 512);
    assert_eq!(result.last().unwrap().code, "observation_limit");
    let resource = Page {
        url: "https://example.com/image.png".into(),
        status: Some(200),
        ..Page::default()
    };
    assert_eq!(
        referenced_resource("https://example.com/a", "/image.png#preview", &[resource]),
        ("observed", Some(200))
    );
}

#[test]
fn offer_alternatives_and_unexpanded_references_are_distinct() {
    let data = block(json!({"@graph":[
        {"@type":"Product", "name":"One", "offers":{"@type":"Offer", "priceSpecification":{"price":5}}},
        {"@type":"Product", "name":"Two", "offers":{"@type":"Offer"}},
        {"@type":"Product", "name":"Three", "offers":{"@type":"Offer", "priceSpecification":{"@id":"#price"}}},
        {"@type":"Product", "name":"Four", "offers":{"@type":"AggregateOffer", "lowPrice":5}}
    ]}));
    let result = validate(&page(), &[data]);
    assert!(
        !result
            .iter()
            .any(|o| o.code == "missing_offer_price" && o.path == "/0/data/@graph/0/offers/price")
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "missing_offer_price" && o.path == "/0/data/@graph/1/offers/price")
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "price_specification_not_expanded")
    );
    assert!(result.iter().any(
        |o| o.code == "missing_aggregate_offer_property" && o.path.ends_with("/priceCurrency")
    ));
}

#[test]
fn microdata_heading_candidates_point_to_source_and_missing_visibility_is_unknown() {
    let item = StructuredData {
        format: "microdata".into(),
        data: Some(
            json!({"types":["https://schema.org/Article"],"properties":{"headline":["Different"]}}),
        ),
        error: None,
    };
    let p = Page {
        headings: vec![(1, "Actual heading".into())],
        ..page()
    };
    let result = validate(&p, std::slice::from_ref(&item));
    assert!(
        result
            .iter()
            .any(|o| o.code == "headline_heading_mismatch_candidate"
                && o.path == "/0/data/properties/headline/0")
    );
    assert!(
        validate(&page(), &[item])
            .iter()
            .any(|o| o.code == "visible_source_unavailable")
    );
}

#[test]
fn image_object_resource_only_uses_observed_inventory() {
    use seosmith::structured_validation::validate_crawl;
    let p = Page {
        structured_data: vec![block(json!({"@type":"ImageObject","url":"/image.png"}))],
        ..page()
    };
    let resource = Page {
        url: "https://example.com/image.png".into(),
        status: Some(404),
        ..Page::default()
    };
    let result = validate_crawl(&p, &[resource]);
    assert!(
        result
            .iter()
            .any(|o| o.path == "/0/data/url" && o.explanation.contains("status=404"))
    );
}

#[test]
fn breadcrumb_position_and_local_address_are_checked_without_assuming_references() {
    let result = validate(
        &page(),
        &[block(json!({"@graph":[
            {"@type":"BreadcrumbList","itemListElement":[{"@type":"ListItem","name":"A","position":0}]},
            {"@type":"LocalBusiness","name":"Shop","address":{"@id":"#address"}},
            {"@id":"#address","@type":"PostalAddress"}
        ]}))],
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "invalid_breadcrumb_position" && o.path.ends_with("/position"))
    );
    assert!(result.iter().any(|o| o.code == "local_address_resolved"));
    assert!(!result.iter().any(|o| o.code == "address_type_candidate"));
}

#[test]
fn offer_reference_resolves_only_a_unique_local_definition() {
    let unique = block(
        json!({"@graph":[{"@type":"Product","name":"Item","offers":{"@id":"#offer"}}, {"@id":"#offer","@type":"Offer"}]}),
    );
    let result = validate(&page(), &[unique]);
    assert!(
        result
            .iter()
            .any(|o| o.code == "local_offer_resolved" && o.path == "/0/data/@graph/0/offers")
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "missing_offer_price" && o.path == "/0/data/@graph/1/price")
    );
    let ambiguous = block(
        json!({"@graph":[{"@type":"Product","name":"Item","offers":{"@id":"#offer"}}, {"@id":"#offer","@type":"Offer"}, {"@id":"#offer","price":5}]}),
    );
    let result = validate(&page(), &[ambiguous]);
    assert!(
        result
            .iter()
            .any(|o| o.code == "referenced_offer_not_expanded")
    );
    assert!(!result.iter().any(|o| o.code == "missing_offer_price"));
}

#[test]
fn custom_context_is_unknown_instead_of_schema_profile_failure() {
    let result = validate(
        &page(),
        &[block(
            json!({"@context":{"Product":"https://example.org/Other"}, "@type":"Product"}),
        )],
    );
    assert!(result.iter().any(|o| o.code == "unsupported_context"));
    assert!(!result.iter().any(|o| o.code == "missing_required"));
}

#[test]
fn price_specification_reference_resolution_preserves_unknowns() {
    let unique = block(
        json!({"@graph":[{"@type":"Product","name":"Item","offers":{"@type":"Offer","priceSpecification":{"@id":"#price"}}},{"@id":"#price","@type":"UnitPriceSpecification","price":12}]}),
    );
    let result = validate(&page(), &[unique]);
    assert!(
        result
            .iter()
            .any(|o| o.code == "local_price_specification_resolved")
    );
    assert!(!result.iter().any(|o| o.code == "missing_offer_price"));
    let missing = block(
        json!({"@type":"Product","name":"Item","offers":{"@type":"Offer","priceSpecification":{"@id":"#unknown"}}}),
    );
    let result = validate(&page(), &[missing]);
    assert!(
        result
            .iter()
            .any(|o| o.code == "price_specification_not_expanded")
    );
    assert!(!result.iter().any(|o| o.code == "missing_offer_price"));
}

#[test]
fn saved_report_cli_emits_sidecar_without_database_or_report_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("report.json");
    let database = dir.path().join("should-not-exist.sqlite");
    let mut report: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/contracts/legacy.json")).unwrap();
    report["pages"][0]["structured_data"] = json!([{"format":"json_ld","data":{"@context":"https://schema.org","@type":"Product","name":"Widget"},"error":null}]);
    let input = serde_json::to_vec(&report).unwrap();
    std::fs::write(&path, &input).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .args([
            "--database",
            database.to_str().unwrap(),
            "validate-structured",
            path.to_str().unwrap(),
        ])
        .env_remove("SEOSMITH_ADVISORY_POLICY")
        .env_remove("SEOSMITH_EVIDENCE_CONTRACT_V2")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sidecar: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        sidecar["profile_version"],
        seosmith::structured_validation::PROFILE_VERSION
    );
    assert!(
        sidecar["structured_validation"][0]["observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["code"] == "missing_product_evidence")
    );
    assert_eq!(std::fs::read(path).unwrap(), input);
    assert!(!database.exists());
}

#[test]
fn blocked_page_cannot_report_structured_data_absence_as_validated() {
    let p = Page {
        blocked_by_robots: true,
        ..page()
    };
    let result = seosmith::structured_validation::validate_crawl(&p, &[]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].code, "page_unavailable");
}

#[test]
fn offer_value_checks_report_precise_array_paths() {
    let result = validate(
        &page(),
        &[block(
            json!({"@type":"Product","name":"X","offers":{"@type":"AggregateOffer","lowPrice":["4.00",-2],"priceCurrency":"usd"}}),
        )],
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "invalid_offer_value" && o.path == "/0/data/offers/lowPrice/1")
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "invalid_offer_value" && o.path == "/0/data/offers/priceCurrency")
    );
    assert!(
        !result
            .iter()
            .any(|o| o.path == "/0/data/offers/lowPrice/0" && o.code == "invalid_offer_value")
    );
}

#[test]
fn output_byte_budget_has_explicit_coverage_marker() {
    let values: Vec<_> = (0..200).map(|_| json!({"@type":"Product", "name":"x", "image":"/x", "description":"x", "offers":{"@type":"Offer", "price":"x".repeat(3000)}})).collect();
    let result = validate(&page(), &[block(json!(values))]);
    assert!(serde_json::to_vec(&result).unwrap().len() <= 512 * 1024);
    assert!(result.iter().any(|o| o.code == "observation_byte_limit"));
}

#[test]
fn article_authors_check_local_definitions_and_unknowns() {
    let result = validate(
        &page(),
        &[block(
            json!({"@graph":[{"@type":"Article","author":[{"@id":"#alice"},{"@id":"#unknown"}]},{"@id":"#alice","@type":"Person"}]}),
        )],
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "local_author_resolved" && o.path == "/0/data/@graph/0/author/0")
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "missing_recommended_author_name"
                && o.path == "/0/data/@graph/1/name")
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "author_not_expanded" && o.path == "/0/data/@graph/0/author/1")
    );
}

#[test]
fn supported_local_business_subtype_uses_business_profile() {
    let result = validate(
        &page(),
        &[block(
            json!({"@context":"https://schema.org", "@type":"Restaurant", "name":"Example"}),
        )],
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "missing_required" && o.path == "/0/data/address")
    );
}

#[test]
fn local_sidecar_distinguishes_observed_partial_and_unavailable() {
    use seosmith::structured_validation::local_sidecar;
    let observed = Page {
        headings: vec![(1, "Title".into())],
        ..page()
    };
    let sidecar = local_sidecar(&observed, &[]);
    assert_eq!(sidecar.state, "observed");
    assert_eq!(sidecar.omitted, Some(0));
    assert_eq!(sidecar.reason, None);
    assert_eq!(sidecar.collector.id, "s04-structured-data");
    let partial = Page {
        structured_data: vec![block(json!({"@type":"ImageObject", "url":"/not-fetched"}))],
        ..observed
    };
    assert_eq!(local_sidecar(&partial, &[]).state, "partial");
    assert_eq!(
        local_sidecar(&partial, &[]).reason,
        Some("incomplete_evidence")
    );
    let blocked = Page {
        blocked_by_robots: true,
        ..page()
    };
    assert_eq!(local_sidecar(&blocked, &[]).state, "unavailable");
    assert_eq!(local_sidecar(&blocked, &[]).omitted, None);
    assert_eq!(
        local_sidecar(&blocked, &[]).reason,
        Some("source_unavailable")
    );
}

#[test]
fn invalid_base_preserves_independent_observations() {
    let mut p = page();
    p.url = "invalid url".into();
    p.extraction_truncated = true;
    let observations = validate(
        &p,
        &[StructuredData {
            format: "json_ld".into(),
            data: Some(json!({"@type":"Product", "name":["Good",42]})),
            error: Some("bad syntax".into()),
        }],
    );
    for code in [
        "base_url_unavailable",
        "invalid_source",
        "extraction_truncated",
    ] {
        assert!(observations.iter().any(|o| o.code == code), "{code}");
    }
    assert!(
        observations
            .iter()
            .any(|o| o.code == "unexpected_value_type"
                && o.path == "/0/data/name/1"
                && o.value == Some(json!(42)))
    );
}

#[test]
fn nested_custom_contexts_are_not_interpreted_by_any_traversal() {
    let mut p = page();
    p.headings = vec![(1, "Visible".into())];
    p.structured_data = vec![block(
        json!({"@type":"Product", "name":"Good", "offers":{"@context":{"price":"other"}, "@type":"Offer", "headline":"Hidden", "image":"/x"}}),
    )];
    let observations = seosmith::structured_validation::validate_crawl(&p, &[p.clone()]);
    assert!(observations.iter().any(|o| o.code == "unsupported_context"));
    for code in [
        "missing_offer_price",
        "headline_heading_mismatch_candidate",
        "referenced_resource_status",
    ] {
        assert!(!observations.iter().any(|o| o.code == code), "{code}");
    }
    let data = block(
        json!({"@type":"Product", "name":"Good", "offers":{"@type":"Offer", "priceSpecification":{"@context":{"price":"other"}, "price":1}}}),
    );
    assert!(
        !validate(&p, &[data])
            .iter()
            .any(|o| o.code == "missing_offer_price")
    );
}

#[test]
fn saved_report_cli_rejects_unknown_schema_and_missing_collection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("report.json");
    let mut report: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/contracts/legacy.json")).unwrap();
    for invalid_schema in [true, false] {
        let mut input = report.clone();
        if invalid_schema {
            input["schema_version"] = json!(999);
        } else {
            input["pages"][0]
                .as_object_mut()
                .unwrap()
                .remove("structured_data");
        }
        std::fs::write(&path, serde_json::to_vec(&input).unwrap()).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_seosmith"))
            .args(["validate-structured", path.to_str().unwrap()])
            .env_remove("SEOSMITH_ADVISORY_POLICY")
            .env_remove("SEOSMITH_EVIDENCE_CONTRACT_V2")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(if invalid_schema {
                "unsupported report schema"
            } else {
                "lacks a structured_data collection"
            })
        );
    }
    report["pages"][0]["structured_data"] = json!([]);
    std::fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
    assert!(
        std::process::Command::new(env!("CARGO_BIN_EXE_seosmith"))
            .args(["validate-structured", path.to_str().unwrap()])
            .env_remove("SEOSMITH_ADVISORY_POLICY")
            .env_remove("SEOSMITH_EVIDENCE_CONTRACT_V2")
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn microdata_heading_candidate_is_not_duplicated() {
    let mut p = page();
    p.headings = vec![(1, "Visible".into())];
    let result = validate(
        &p,
        &[block(
            json!({"types":["https://schema.org/Article"],"properties":{"headline":["Different"]}}),
        )],
    );
    assert_eq!(
        result
            .iter()
            .filter(|o| o.code == "headline_heading_mismatch_candidate"
                && o.path == "/0/data/properties/headline/0")
            .count(),
        1
    );
}

#[test]
fn breadcrumb_item_references_preserve_name_evidence_and_unknowns() {
    let data = block(
        json!({"@graph":[{"@type":"BreadcrumbList","itemListElement":[{"@type":"ListItem","position":1,"item":{"@id":"#page"}},{"@type":"ListItem","position":2,"item":{"@id":"#missing"}}]},{"@id":"#page","@type":"WebPage","name":"Home"}]}),
    );
    let result = validate(&page(), &[data]);
    assert!(
        !result
            .iter()
            .any(|o| o.code == "missing_breadcrumb_property" && o.path.ends_with("/name"))
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "breadcrumb_item_name_unknown" && o.path.ends_with("/1/item"))
    );
}

#[test]
fn offer_currency_is_checked_without_assuming_price_specification() {
    let data = block(
        json!({"@type":"Product","name":"X","offers":[{"@type":"Offer","price":5},{"@type":"Offer","priceSpecification":{"price":5,"priceCurrency":"USD"}}]}),
    );
    let result = validate(&page(), &[data]);
    assert!(
        result
            .iter()
            .any(|o| o.code == "missing_offer_currency"
                && o.path == "/0/data/offers/0/priceCurrency")
    );
    assert!(
        result.iter().any(|o| o.code == "offer_currency_unknown"
            && o.path == "/0/data/offers/1/priceSpecification")
    );
    assert!(
        !result
            .iter()
            .any(|o| o.code == "missing_offer_currency" && o.path.contains("/offers/1/"))
    );
}

#[test]
fn saved_v2_reports_validate_retained_pages_and_preserve_input() {
    for fixture in [
        include_bytes!("../fixtures/contracts/v2-complete.json").as_slice(),
        include_bytes!("../fixtures/contracts/v2-partial.json").as_slice(),
        include_bytes!("../fixtures/contracts/v2-truncated.json").as_slice(),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        let database = dir.path().join("absent.sqlite");
        std::fs::write(&path, fixture).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_seosmith"))
            .args([
                "--database",
                database.to_str().unwrap(),
                "validate-structured",
                path.to_str().unwrap(),
            ])
            .env_remove("SEOSMITH_ADVISORY_POLICY")
            .env_remove("SEOSMITH_EVIDENCE_CONTRACT_V2")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let source: serde_json::Value = serde_json::from_slice(fixture).unwrap();
        let sidecar: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let pages = source["pages"].as_array().unwrap();
        let results = sidecar["structured_validation"].as_array().unwrap();
        assert_eq!(results.len(), pages.len());
        for (i, (page, result)) in pages.iter().zip(results).enumerate() {
            assert_eq!(result["page_index"], i);
            assert_eq!(result["url"], page["url"]);
            if page["status"].is_null()
                || page["blocked_by_robots"] == true
                || page["is_resource"] == true
            {
                assert!(
                    result["observations"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|o| o["code"] == "page_unavailable")
                );
            }
        }
        assert_eq!(std::fs::read(path).unwrap(), fixture);
        assert!(!database.exists());
    }
}

#[test]
fn saved_v2_rejects_unknown_contract_and_missing_collection() {
    let original: serde_json::Value =
        serde_json::from_slice(include_bytes!("../fixtures/contracts/v2-complete.json")).unwrap();
    for case in 0..4 {
        let mut report = original.clone();
        match case {
            0 => report["evidence_contract"]["version"] = json!(2),
            1 => {
                report.as_object_mut().unwrap().remove("evidence_contract");
            }
            2 => {
                report["pages"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("structured_data");
            }
            _ => {
                report["evidence_contract"]
                    .as_object_mut()
                    .unwrap()
                    .remove("coverage");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        std::fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_seosmith"))
            .args(["validate-structured", path.to_str().unwrap()])
            .env_remove("SEOSMITH_ADVISORY_POLICY")
            .env_remove("SEOSMITH_EVIDENCE_CONTRACT_V2")
            .output()
            .unwrap();
        assert!(!output.status.success(), "case {case}");
    }
}

#[test]
fn saved_v2_uses_page_evidence_even_when_contract_is_truncated() {
    let mut report: serde_json::Value =
        serde_json::from_slice(include_bytes!("../fixtures/contracts/v2-truncated.json")).unwrap();
    report["pages"][0]["status"] = json!(200);
    report["pages"][0]["content_type"] = json!("text/html");
    report["pages"][0]["extraction_truncated"] = json!(true);
    report["pages"][0]["structured_data"] =
        json!([{"format":"json_ld", "data":{"@type":"Product", "name":"Widget"}, "error":null}]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("report.json");
    std::fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_seosmith"))
            .args(["validate-structured", path.to_str().unwrap()])
            .env_remove("SEOSMITH_ADVISORY_POLICY")
            .env_remove("SEOSMITH_EVIDENCE_CONTRACT_V2")
            .output()
            .unwrap()
    };
    let v2 = run();
    assert!(
        v2.status.success(),
        "{}",
        String::from_utf8_lossy(&v2.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&v2.stdout).unwrap();
    let observations = result["structured_validation"][0]["observations"]
        .as_array()
        .unwrap();
    assert!(
        observations
            .iter()
            .any(|o| o["code"] == "missing_product_evidence")
    );
    assert!(
        observations
            .iter()
            .any(|o| o["code"] == "extraction_truncated")
    );
    report["schema_version"] = json!(1);
    report.as_object_mut().unwrap().remove("evidence_contract");
    std::fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
    let v1 = run();
    assert!(v1.status.success());
    assert_eq!(v1.stdout, v2.stdout);
}

/// Representative checked-in valid markup must not produce syntax, vocabulary or
/// provider-eligibility assertions. Source pointers, coverage, human-review and
/// already-observed resource status remain explicit.
#[test]
fn checked_in_positive_fixture_is_clean_and_path_scoped() {
    use seosmith::model::Report;
    use seosmith::structured_validation::validate_crawl;
    let report: Report =
        serde_json::from_str(include_str!("../fixtures/s04/positive.json")).unwrap();
    for (index, page) in report.pages.iter().enumerate().take(4) {
        let observations = validate_crawl(page, &report.pages);
        assert!(
            observations
                .iter()
                .any(|o| o.code == "context_not_expanded" && o.category == "coverage"),
            "page {index} lacks explicit context coverage"
        );
        assert!(
            observations
                .iter()
                .any(|o| o.category == "human_review" && o.code == "visible_content_unverified"),
            "page {index} lacks the human-review condition"
        );
        assert!(
            !observations
                .iter()
                .any(|o| matches!(o.category, "syntax" | "vocabulary" | "provider_eligibility")),
            "positive page {index} unexpectedly produced assertions: {observations:?}"
        );
    }
    // An image already present in the retained crawl inventory is observed; the
    // pointer is the RFC 6901 item/property path relative to `structured_data`.
    let product = &report.pages[0];
    let observations = validate_crawl(product, &report.pages);
    assert!(observations.iter().any(|o| o.category == "resource"
        && o.code == "referenced_resource_status"
        && o.path == "/0/data/image"
        && o.explanation.contains("state=observed")
        && o.explanation.contains("status=200")));
    // Pages that were never fetched stay unavailable rather than validated-empty.
    for index in [4, 5] {
        let unavailable = validate_crawl(&report.pages[index], &report.pages);
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0].code, "page_unavailable");
    }
}

/// The negative fixture separates JSON syntax, vocabulary, provider eligibility
/// and human-review conditions at their precise item/property pointers.
#[test]
fn checked_in_negative_fixture_separates_observation_categories() {
    use seosmith::model::Report;
    use seosmith::structured_validation::validate_crawl;
    let report: Report =
        serde_json::from_str(include_str!("../fixtures/s04/negative.json")).unwrap();
    let has = |index: usize, category: &str, code: &str, path: &str| {
        validate_crawl(&report.pages[index], &report.pages)
            .iter()
            .any(|o| o.category == category && o.code == code && o.path == path)
    };
    // Syntax is isolated from the rest.
    assert!(has(5, "syntax", "invalid_source", "/0/error"));
    // Vocabulary checks report the exact item/property pointer and retained value.
    assert!(has(
        1,
        "vocabulary",
        "unexpected_value_type",
        "/0/data/name"
    ));
    assert!(has(
        0,
        "vocabulary",
        "invalid_offer_value",
        "/0/data/offers/price"
    ));
    assert!(has(
        0,
        "vocabulary",
        "invalid_offer_value",
        "/0/data/offers/priceCurrency"
    ));
    assert!(has(
        3,
        "vocabulary",
        "invalid_breadcrumb_position",
        "/0/data/itemListElement/0/position"
    ));
    // Provider eligibility and local-reference states stay distinct.
    assert!(has(
        0,
        "provider_eligibility",
        "missing_required",
        "/0/data/name"
    ));
    assert!(has(
        4,
        "provider_eligibility",
        "missing_required",
        "/0/data/address"
    ));
    assert!(has(2, "coverage", "author_not_expanded", "/0/data/author"));
    assert!(has(
        6,
        "coverage",
        "unsupported_context",
        "/0/data/@context"
    ));
    // Visible-content alignment is always a human-review candidate, never a verdict.
    assert!(has(
        2,
        "human_review",
        "headline_heading_mismatch_candidate",
        "/0/data/headline"
    ));
    // A blocked page is explicitly unavailable, never a clean empty validation.
    let blocked = validate_crawl(&report.pages[7], &report.pages);
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].code, "page_unavailable");
}

/// Both checked-in fixtures run through the normal saved-report entry point
/// without opening a database or mutating the source report.
#[test]
fn checked_in_s04_fixtures_run_through_cli_without_mutation() {
    for fixture in [
        include_bytes!("../fixtures/s04/positive.json").as_slice(),
        include_bytes!("../fixtures/s04/negative.json").as_slice(),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        let database = dir.path().join("absent.sqlite");
        std::fs::write(&path, fixture).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_seosmith"))
            .args([
                "--database",
                database.to_str().unwrap(),
                "validate-structured",
                path.to_str().unwrap(),
            ])
            .env_remove("SEOSMITH_ADVISORY_POLICY")
            .env_remove("SEOSMITH_EVIDENCE_CONTRACT_V2")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let source: serde_json::Value = serde_json::from_slice(fixture).unwrap();
        let sidecar: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            sidecar["profile_version"],
            seosmith::structured_validation::PROFILE_VERSION
        );
        assert_eq!(
            sidecar["structured_validation"].as_array().unwrap().len(),
            source["pages"].as_array().unwrap().len()
        );
        assert_eq!(std::fs::read(&path).unwrap(), fixture);
        assert!(!database.exists());
    }
}

/// A node carrying a local `@id` is completed from one separate retained
/// definition, so split Product and Offer nodes do not produce false missing
/// assertions. Ambiguous or foreign identifiers are never merged.
#[test]
fn local_id_node_merging_completes_split_definitions_without_false_assertions() {
    // A typed inline Product completed by a separate name/image definition.
    let product = validate(
        &page(),
        &[block(json!({"@graph":[
            {"@type":"Product", "@id":"#p", "offers":{"@type":"Offer", "price":5, "priceCurrency":"USD"}},
            {"@id":"#p", "name":"Widget", "description":"A widget", "image":"/i.png"}
        ]}))],
    );
    assert!(product.iter().any(|o| o.code == "local_definition_merged"));
    assert!(!product.iter().any(|o| o.code == "missing_required"));
    assert!(!product.iter().any(|o| o.code == "missing_recommended"));

    // An inline Offer completed by a separate currency definition.
    let offer = validate(
        &page(),
        &[block(json!({"@graph":[
            {"@type":"Product", "name":"X", "offers":{"@id":"#offer", "@type":"Offer", "price":5}},
            {"@id":"#offer", "priceCurrency":"USD"}
        ]}))],
    );
    assert!(offer.iter().any(|o| o.code == "local_definition_merged"));
    assert!(!offer.iter().any(|o| o.code == "missing_offer_currency"));
    assert!(!offer.iter().any(|o| o.code == "missing_offer_price"));

    // A breadcrumb ListItem completed by a separate position/name definition.
    let breadcrumb = validate(
        &page(),
        &[block(json!({"@graph":[
            {"@type":"BreadcrumbList", "itemListElement":[{"@id":"#li", "@type":"ListItem"}]},
            {"@id":"#li", "position":1, "name":"Home"}
        ]}))],
    );
    assert!(
        !breadcrumb
            .iter()
            .any(|o| o.code == "missing_breadcrumb_property")
    );
    assert!(
        !breadcrumb
            .iter()
            .any(|o| o.code == "invalid_breadcrumb_position")
    );

    // Two separate definitions stay ambiguous and are not merged, so the
    // honestly missing Product name is still reported.
    let ambiguous = validate(
        &page(),
        &[block(json!({"@graph":[
            {"@type":"Product", "@id":"#p", "offers":{"@type":"Offer", "price":5, "priceCurrency":"USD"}},
            {"@id":"#p", "name":"A"},
            {"@id":"#p", "name":"B"}
        ]}))],
    );
    assert!(
        !ambiguous
            .iter()
            .any(|o| o.code == "local_definition_merged")
    );
    assert!(
        ambiguous
            .iter()
            .any(|o| o.code == "missing_required" && o.path.ends_with("/name"))
    );

    // A foreign identifier is never merged into the local node.
    let foreign = validate(
        &page(),
        &[block(json!({"@graph":[
            {"@type":"Product", "@id":"https://other.example/p", "offers":{"@type":"Offer", "price":5, "priceCurrency":"USD"}},
            {"@id":"https://other.example/p", "name":"Remote"}
        ]}))],
    );
    assert!(!foreign.iter().any(|o| o.code == "local_definition_merged"));
    assert!(
        foreign
            .iter()
            .any(|o| o.code == "missing_required" && o.path.ends_with("/name"))
    );
}

/// Values completed from a separate local definition are reported at the
/// definition's actual occurrence, never at a nonexistent pointer on the node.
#[test]
fn merged_values_report_their_actual_definition_pointer() {
    // Split Product: the description and offers live only in the definition.
    let product = validate(
        &page(),
        &[block(json!({"@graph":[
            {"@type":"Product", "@id":"#p", "name":"Widget"},
            {"@id":"#p",
             "description":42,
             "offers":{"@type":"Offer", "price":"abc", "priceCurrency":"usd"}}
        ]}))],
    );
    assert!(product.iter().any(|o| o.code == "unexpected_value_type"
        && o.path == "/0/data/@graph/1/description"
        && o.value == Some(json!(42))));
    assert!(
        !product
            .iter()
            .any(|o| o.code == "unexpected_value_type" && o.path.contains("@graph/0/description"))
    );
    assert!(
        product
            .iter()
            .any(|o| o.code == "invalid_offer_value" && o.path == "/0/data/@graph/1/offers/price")
    );
    assert!(
        product.iter().any(|o| o.code == "invalid_offer_value"
            && o.path == "/0/data/@graph/1/offers/priceCurrency")
    );
    assert!(
        !product
            .iter()
            .any(|o| o.path.contains("@graph/0/offers/price"))
    );

    // A ListItem position completed from its definition keeps the definition path.
    let breadcrumb = validate(
        &page(),
        &[block(json!({"@graph":[
            {"@type":"BreadcrumbList", "itemListElement":[{"@type":"ListItem", "@id":"#li"}]},
            {"@id":"#li", "position":0}
        ]}))],
    );
    assert!(
        breadcrumb
            .iter()
            .any(|o| o.code == "invalid_breadcrumb_position"
                && o.path == "/0/data/@graph/1/position")
    );
    assert!(
        !breadcrumb
            .iter()
            .any(|o| o.path == "/0/data/@graph/0/itemListElement/0/position")
    );
}

/// Two typed declarations sharing an `@id` are a duplicate or conflicting
/// declaration, not a definition: neither is completed and the conflict is
/// reported instead of silently merging cross-block properties.
#[test]
fn symmetric_typed_declarations_are_ambiguous_and_not_merged() {
    let result = validate(
        &page(),
        &[block(json!({"@graph":[
            {"@type":"Product", "@id":"#p", "offers":{"@type":"Offer", "price":5, "priceCurrency":"USD"}},
            {"@type":"Article", "@id":"#p", "headline":7}
        ]}))],
    );
    assert!(result.iter().any(|o| o.category == "coverage"
        && o.code == "local_definition_ambiguous"
        && o.path == "/0/data/@graph/0"));
    assert!(
        result
            .iter()
            .any(|o| o.code == "local_definition_ambiguous" && o.path == "/0/data/@graph/1")
    );
    assert!(!result.iter().any(|o| o.code == "local_definition_merged"));
    // The Product must not inherit and re-check the Article headline.
    assert!(
        !result
            .iter()
            .any(|o| o.code == "unexpected_value_type" && o.path == "/0/data/@graph/0/headline")
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "unexpected_value_type" && o.path == "/0/data/@graph/1/headline")
    );
    assert!(
        result
            .iter()
            .any(|o| o.code == "missing_required" && o.path == "/0/data/@graph/0/name")
    );
}
