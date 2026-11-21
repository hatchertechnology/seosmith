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
