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
