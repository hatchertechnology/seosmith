//! Bounded static resource delivery and affected-page analysis for saved local checkpoints.
//!
//! Inputs are the retained `resource` and `resource_references` collectors plus the
//! saved page relationship graph. Missing measurements stay unknown; they are never
//! converted to zero bytes. No fetch, persistence, report or delivery is performed.
use crate::{
    model::{Page, Relationship, normalize},
    resource_evidence,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

pub const PROFILE_VERSION: u32 = 1;
pub const SEMANTICS: &str = "static_local_resource_delivery_impact_v1";
const MAX_URL_COPY: usize = 2048;
const MAX_REFERRERS_TOTAL: usize = 5000;
const MAX_REFERRERS_PER_RESOURCE: usize = 100;
const MAX_AFFECTED_PER_RESOURCE: usize = 500;
const MAX_AFFECTED_TOTAL: usize = 5000;
const MAX_RAW_CHARS: usize = 512;
const MAX_DESCRIPTOR_CHARS: usize = 256;
const MAX_REASON_CHARS: usize = 256;

/// Decoded body measurements as serialized by the version 1 local `resource` collector.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedResourceMeasurement {
    pub url: String,
    pub decoded_body_bytes: u64,
    pub declared_content_length: Option<u64>,
    pub transfer_bytes: Option<u64>,
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    pub cache_control: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub body_hash: String,
    pub detected_image_format: Option<String>,
    pub image_dimensions: Option<(u32, u32)>,
}

/// Retained static HTML occurrences from the version 1 `resource_references` collector.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedResourceReferences {
    pub source_url: String,
    pub occurrences: Vec<SavedReferenceOccurrence>,
    pub complete: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedReferenceOccurrence {
    pub element: String,
    pub raw_src: Option<String>,
    pub raw_rel: Option<String>,
    pub raw_srcset: Option<String>,
    pub media: Option<String>,
    pub declared_type: Option<String>,
    pub as_attribute: Option<String>,
    pub crossorigin: Option<String>,
    pub sizes: Option<String>,
    pub loading: Option<String>,
    pub fetchpriority: Option<String>,
    pub width: Option<String>,
    pub height: Option<String>,
}

/// Explicit collector outcome, including absent and unreadable evidence.
#[derive(Debug, Clone)]
pub enum CollectorValue<T> {
    Absent,
    Unavailable { reason: Option<String> },
    Partial(T),
    Observed(T),
    Invalid { reason: &'static str },
}

impl<T> CollectorValue<T> {
    fn data(&self) -> Option<&T> {
        match self {
            Self::Partial(value) | Self::Observed(value) => Some(value),
            Self::Absent | Self::Unavailable { .. } | Self::Invalid { .. } => None,
        }
    }
    fn state(&self) -> (&'static str, Option<String>) {
        match self {
            Self::Absent => ("absent", None),
            Self::Unavailable { reason } => ("unavailable", reason.clone()),
            Self::Partial(_) => ("partial", None),
            Self::Observed(_) => ("observed", None),
            Self::Invalid { reason } => ("invalid", Some((*reason).to_owned())),
        }
    }
    fn is_partial(&self) -> bool {
        matches!(self, Self::Partial(_))
    }
}

/// One bounded frontier row and the collector payloads selected from its checkpoint.
#[derive(Debug, Clone)]
pub struct SavedPageRow {
    pub url: String,
    pub is_resource: bool,
    pub fetched: bool,
    pub page: Option<Page>,
    pub page_omission: Option<&'static str>,
    pub resource: CollectorValue<SavedResourceMeasurement>,
    pub references: CollectorValue<SavedResourceReferences>,
}

pub struct ImpactInput<'a> {
    pub rows: &'a [SavedPageRow],
    pub input_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct UrlValue {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url_omitted: Option<&'static str>,
    pub url_sha256: String,
}

fn url_value(value: &str) -> UrlValue {
    if value.chars().count() <= MAX_URL_COPY {
        UrlValue {
            url: Some(value.to_owned()),
            url_omitted: None,
            url_sha256: crate::model::digest(value.as_bytes()),
        }
    } else {
        UrlValue {
            url: None,
            url_omitted: Some("output_url_copy_limit"),
            url_sha256: crate::model::digest(value.as_bytes()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Field<T> {
    pub value: Option<T>,
    pub semantics: &'static str,
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn field<T>(value: Option<T>, semantics: &'static str, source: &'static str) -> Field<T> {
    Field {
        value,
        semantics,
        source,
        reason: None,
    }
}

fn unknown<T>(
    semantics: &'static str,
    source: &'static str,
    reason: impl Into<String>,
) -> Field<T> {
    Field {
        value: None,
        semantics,
        source,
        reason: Some(reason.into()),
    }
}

fn reason_text(value: Option<&str>) -> String {
    value.map_or_else(
        || "unspecified".into(),
        |v| v.chars().take(MAX_REASON_CHARS).collect(),
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseState {
    pub state: &'static str,
    pub status: Option<u16>,
    pub http_success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MeasurementState {
    pub state: &'static str,
    pub collector_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceBytes {
    pub decoded_body: Field<u64>,
    pub declared_content_length: Field<u64>,
    pub transfer: Field<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MimeObservation {
    pub declared: Field<String>,
    pub detected_signature: Field<String>,
    pub image_dimensions: Field<(u32, u32)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompressionObservation {
    pub content_encoding: Field<String>,
    pub transport_decoded_body: Field<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CacheObservation {
    pub cache_control: Field<String>,
    pub etag: Field<String>,
    pub last_modified: Field<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Referrer {
    pub page: UrlValue,
    pub provenance: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribute: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_descriptor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sizes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loading: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetchpriority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crossorigin: Option<String>,
    pub reference_state: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReferrerSummary {
    pub coverage: &'static str,
    pub observed_referrers: usize,
    pub omitted_referrers: usize,
    pub items: Vec<Referrer>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AffectedPages {
    pub semantics: &'static str,
    pub coverage: &'static str,
    pub pages: Vec<UrlValue>,
    pub omitted_pages: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceImpactEntry {
    pub url: UrlValue,
    pub response: ResponseState,
    pub measurement: MeasurementState,
    pub bytes: ResourceBytes,
    pub body_sha256: Field<String>,
    pub mime: MimeObservation,
    pub compression: CompressionObservation,
    pub cache: CacheObservation,
    pub referrers: ReferrerSummary,
    pub affected_pages: AffectedPages,
    pub duplicate_group_sha256: Option<String>,
    pub counted_in_byte_totals: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusion_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateGroup {
    pub decoded_body_hash: String,
    pub urls: Vec<UrlValue>,
    pub semantics: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ByteSemantics {
    pub decoded_body: &'static str,
    pub declared_content_length: &'static str,
    pub transfer: &'static str,
}

struct ByteAccum {
    counted: usize,
    bytes: u64,
    unknown: usize,
    excluded: usize,
    complete: bool,
}

impl Default for ByteAccum {
    fn default() -> Self {
        Self {
            counted: 0,
            bytes: 0,
            unknown: 0,
            excluded: 0,
            complete: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ByteGroup {
    pub origin: Option<String>,
    pub mime: Option<String>,
    pub counted_resources: usize,
    /// Sum of the counted members only, or `None` when no member was counted.
    /// Excluded and unknown members contribute nothing, so a non-`None` value is
    /// a lower bound on the delivered bytes unless `complete` is true.
    pub decoded_body_bytes: Option<u64>,
    pub unknown_decoded_bytes_resources: usize,
    pub excluded_resources: usize,
    /// False when any member is excluded, unknown, partial, truncated or lost to
    /// a row limit, so the group sum is then only a lower bound.
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ByteSummary {
    pub semantics: ByteSemantics,
    pub counted_resources: usize,
    /// Sum of the counted resources only, or `None` when nothing was counted.
    /// Excluded and unknown resources contribute nothing, so a non-`None` value
    /// is a lower bound on the delivered bytes unless `complete` is true.
    pub decoded_body_bytes: Option<u64>,
    pub unknown_decoded_bytes_resources: usize,
    pub excluded_resources: usize,
    /// False when any resource is excluded, unknown, partial, truncated or lost
    /// to a row limit, so the totals are then only a lower bound.
    pub complete: bool,
    pub groups: Vec<ByteGroup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactCoverage {
    pub requested_resources: usize,
    pub measured_resources: usize,
    pub unknown_measurement_resources: usize,
    pub unsuccessful_resources: usize,
    pub not_fetched_resources: usize,
    pub reference_sources_observed: usize,
    pub reference_sources_partial: usize,
    pub reference_sources_unavailable: usize,
    pub reference_sources_absent: usize,
    pub affected_pages_coverage: &'static str,
    pub referrer_coverage: &'static str,
    pub bytes_complete: bool,
    pub input_truncated: bool,
    pub discovery_coverage: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceImpact {
    pub coverage: ImpactCoverage,
    pub bytes: ByteSummary,
    pub resources: Vec<ResourceImpactEntry>,
    pub duplicates: Vec<DuplicateGroup>,
}

#[derive(Default)]
struct ResourceState {
    row_index: usize,
    counted: bool,
    exclusion_reason: Option<&'static str>,
    referrers: Vec<Referrer>,
    referrer_keys: BTreeSet<String>,
    omitted_referrers: usize,
    affected: Vec<String>,
    omitted_affected: usize,
}

fn mime_type(value: &str) -> Option<String> {
    let value = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    (!value.is_empty()).then_some(value)
}

fn url_origin(value: &str) -> Option<String> {
    Url::parse(value)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(|u| u.origin().ascii_serialization())
}

fn clip(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn optional_clip(value: Option<&String>, limit: usize) -> Option<String> {
    value.map(|v| clip(v, limit))
}

fn response_state(row: &SavedPageRow) -> ResponseState {
    if !row.fetched {
        return ResponseState {
            state: "not_fetched",
            status: None,
            http_success: None,
            error: None,
        };
    }
    let Some(page) = &row.page else {
        return ResponseState {
            state: "page_evidence_omitted",
            status: None,
            http_success: None,
            error: None,
        };
    };
    if page.blocked_by_robots {
        return ResponseState {
            state: "blocked_by_robots",
            status: page.status,
            http_success: None,
            error: None,
        };
    }
    if let Some(error) = &page.error {
        return ResponseState {
            state: "fetch_failed",
            status: page.status,
            http_success: None,
            error: Some(clip(error, MAX_REASON_CHARS)),
        };
    }
    match page.status {
        Some(status) if (200..300).contains(&status) => ResponseState {
            state: "fetched_success",
            status: Some(status),
            http_success: Some(true),
            error: None,
        },
        Some(status) => ResponseState {
            state: "fetched_unsuccessful",
            status: Some(status),
            http_success: Some(false),
            error: None,
        },
        None => ResponseState {
            state: "fetched_status_unknown",
            status: None,
            http_success: None,
            error: None,
        },
    }
}

fn measurement_state(value: &CollectorValue<SavedResourceMeasurement>) -> MeasurementState {
    let (state, reason) = value.state();
    MeasurementState {
        state,
        collector_version: 1,
        reason,
    }
}

fn field_or_unknown(
    data: Option<&SavedResourceMeasurement>,
    value: Option<&String>,
    semantics: &'static str,
    source: &'static str,
    absent_reason: &'static str,
    collector_state: &MeasurementState,
) -> Field<String> {
    match data {
        None => unknown(
            "unknown",
            source,
            format!(
                "measurement_{}",
                reason_text(collector_state.reason.as_deref())
            ),
        ),
        Some(_) => match value {
            Some(value) => field(Some(value.clone()), semantics, source),
            None => unknown("unknown", source, absent_reason),
        },
    }
}

fn resource_bytes(
    data: Option<&SavedResourceMeasurement>,
    measurement: &MeasurementState,
) -> ResourceBytes {
    let decoded_body = match data {
        Some(value) => field(
            Some(value.decoded_body_bytes),
            "measured",
            "transport_decoded_body",
        ),
        None => unknown(
            "unknown",
            "transport_decoded_body",
            format!("measurement_{}", reason_text(measurement.reason.as_deref())),
        ),
    };
    let declared_content_length = match data {
        Some(value) => match value.declared_content_length {
            Some(bytes) => field(Some(bytes), "estimated", "content_length_header"),
            None => unknown(
                "unknown",
                "content_length_header",
                "content_length_header_absent_or_invalid",
            ),
        },
        None => unknown(
            "unknown",
            "content_length_header",
            format!("measurement_{}", reason_text(measurement.reason.as_deref())),
        ),
    };
    ResourceBytes {
        decoded_body,
        declared_content_length,
        transfer: unknown(
            "unknown",
            "transport",
            "transfer_size_not_retained_by_local_checkpoint",
        ),
    }
}

fn mime_observation(
    data: Option<&SavedResourceMeasurement>,
    measurement: &MeasurementState,
) -> MimeObservation {
    let declared = field_or_unknown(
        data,
        data.and_then(|v| v.content_type.as_ref()),
        "declared_header",
        "content_type_header",
        "content_type_header_absent",
        measurement,
    );
    let (detected_signature, image_dimensions) = match data {
        Some(value) => (
            match &value.detected_image_format {
                Some(format) => field(
                    Some(format.clone()),
                    "measured_byte_prefix",
                    "transport_body_prefix",
                ),
                None => unknown(
                    "measured_byte_prefix",
                    "transport_body_prefix",
                    "no_supported_image_signature",
                ),
            },
            match value.image_dimensions {
                Some(dimensions) => field(
                    Some(dimensions),
                    "measured_byte_prefix",
                    "transport_body_prefix",
                ),
                None => unknown(
                    "measured_byte_prefix",
                    "transport_body_prefix",
                    "no_supported_image_dimensions",
                ),
            },
        ),
        None => (
            unknown(
                "unknown",
                "transport_body_prefix",
                format!("measurement_{}", reason_text(measurement.reason.as_deref())),
            ),
            unknown(
                "unknown",
                "transport_body_prefix",
                format!("measurement_{}", reason_text(measurement.reason.as_deref())),
            ),
        ),
    };
    MimeObservation {
        declared,
        detected_signature,
        image_dimensions,
    }
}

fn compression_observation(
    data: Option<&SavedResourceMeasurement>,
    measurement: &MeasurementState,
) -> CompressionObservation {
    CompressionObservation {
        content_encoding: field_or_unknown(
            data,
            data.and_then(|v| v.content_encoding.as_ref()),
            "declared_header",
            "content_encoding_header",
            "content_encoding_header_absent_not_proof_of_identity",
            measurement,
        ),
        transport_decoded_body: unknown("unknown", "transport", "transport_decoding_not_retained"),
    }
}

fn cache_observation(
    data: Option<&SavedResourceMeasurement>,
    measurement: &MeasurementState,
) -> CacheObservation {
    CacheObservation {
        cache_control: field_or_unknown(
            data,
            data.and_then(|v| v.cache_control.as_ref()),
            "declared_header",
            "cache_control_header",
            "cache_control_header_absent_not_proof_of_behavior",
            measurement,
        ),
        etag: field_or_unknown(
            data,
            data.and_then(|v| v.etag.as_ref()),
            "declared_header",
            "etag_header",
            "etag_header_absent",
            measurement,
        ),
        last_modified: field_or_unknown(
            data,
            data.and_then(|v| v.last_modified.as_ref()),
            "declared_header",
            "last_modified_header",
            "last_modified_header_absent",
            measurement,
        ),
    }
}

fn reference_state(
    references: &CollectorValue<SavedResourceReferences>,
    data: &SavedResourceReferences,
) -> &'static str {
    if references.is_partial() || !data.complete {
        "partial"
    } else {
        "observed"
    }
}

fn add_referrer(state: &mut ResourceState, total: &mut usize, referrer: Referrer) {
    let key = format!(
        "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        referrer.page.url_sha256,
        referrer.provenance,
        referrer.element.as_deref().unwrap_or_default(),
        referrer.attribute.as_deref().unwrap_or_default(),
        referrer.raw_value.as_deref().unwrap_or_default(),
        referrer.candidate_descriptor.as_deref().unwrap_or_default(),
        referrer.kind.as_deref().unwrap_or_default(),
    );
    if !state.referrer_keys.insert(key) {
        return;
    }
    if state.referrers.len() >= MAX_REFERRERS_PER_RESOURCE || *total >= MAX_REFERRERS_TOTAL {
        state.omitted_referrers = state.omitted_referrers.saturating_add(1);
        return;
    }
    *total += 1;
    state.referrers.push(referrer);
}

fn relationship_referrer(row: &SavedPageRow, relationship: &Relationship) -> Referrer {
    let partial = row.page.as_ref().is_some_and(|p| p.extraction_truncated);
    Referrer {
        page: url_value(&row.url),
        provenance: "page_relationship",
        element: None,
        attribute: None,
        raw_value: None,
        candidate_descriptor: None,
        kind: Some(clip(&relationship.kind, MAX_DESCRIPTOR_CHARS)),
        descriptor: optional_clip(Some(&relationship.descriptor), MAX_DESCRIPTOR_CHARS),
        media: None,
        declared_type: None,
        sizes: None,
        loading: None,
        fetchpriority: None,
        crossorigin: None,
        reference_state: if partial { "partial" } else { "observed" },
    }
}

fn reference_referrer(
    row: &SavedPageRow,
    references: &SavedResourceReferences,
    occurrence: &SavedReferenceOccurrence,
    raw_value: &str,
    attribute: &'static str,
    candidate_descriptor: Option<&str>,
) -> Referrer {
    Referrer {
        page: url_value(&row.url),
        provenance: "static_html_reference",
        element: Some(clip(&occurrence.element, 64)),
        attribute: Some(attribute.into()),
        raw_value: Some(clip(raw_value, MAX_RAW_CHARS)),
        candidate_descriptor: candidate_descriptor.map(|value| clip(value, MAX_DESCRIPTOR_CHARS)),
        kind: optional_clip(occurrence.raw_rel.as_ref(), MAX_DESCRIPTOR_CHARS),
        descriptor: optional_clip(occurrence.as_attribute.as_ref(), MAX_DESCRIPTOR_CHARS),
        media: optional_clip(occurrence.media.as_ref(), MAX_DESCRIPTOR_CHARS),
        declared_type: optional_clip(occurrence.declared_type.as_ref(), MAX_DESCRIPTOR_CHARS),
        sizes: optional_clip(occurrence.sizes.as_ref(), MAX_DESCRIPTOR_CHARS),
        loading: optional_clip(occurrence.loading.as_ref(), MAX_DESCRIPTOR_CHARS),
        fetchpriority: optional_clip(occurrence.fetchpriority.as_ref(), MAX_DESCRIPTOR_CHARS),
        crossorigin: optional_clip(occurrence.crossorigin.as_ref(), MAX_DESCRIPTOR_CHARS),
        reference_state: reference_state(&row.references, references),
    }
}

/// Analyze already-bounded saved rows.
///
/// All relationship and reference resolution uses the saved source page URL as the
/// base. A `<base>` element is not retained, so a saved base override is unknown.
#[must_use]
pub fn analyze(input: &ImpactInput<'_>) -> ResourceImpact {
    let mut accumulators: BTreeMap<String, ResourceState> = BTreeMap::new();
    for (index, row) in input.rows.iter().enumerate() {
        if row.is_resource {
            accumulators.insert(
                row.url.clone(),
                ResourceState {
                    row_index: index,
                    ..ResourceState::default()
                },
            );
        }
    }

    let mut pages = Vec::new();
    let mut affected_coverage_partial = input.input_truncated;
    for row in input.rows {
        let (status, content_type, error, blocked, truncated, relationships) =
            if let Some(page) = &row.page {
                (
                    page.status,
                    page.content_type.clone(),
                    page.error.clone(),
                    page.blocked_by_robots,
                    page.extraction_truncated,
                    page.relationships
                        .iter()
                        .filter(|relationship| relationship.resource)
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            } else {
                if !row.is_resource && row.fetched {
                    affected_coverage_partial = true;
                }
                (None, String::new(), None, false, false, Vec::new())
            };
        if truncated {
            affected_coverage_partial = true;
        }
        pages.push(Page {
            url: row.url.clone(),
            status,
            content_type,
            error,
            blocked_by_robots: blocked,
            relationships,
            extraction_truncated: truncated,
            is_resource: row.is_resource,
            ..Page::default()
        });
    }
    let affected = resource_evidence::affected_pages(&pages);

    let mut total_referrers = 0usize;
    let mut reference_sources_observed = 0usize;
    let mut reference_sources_partial = 0usize;
    let mut reference_sources_unavailable = 0usize;
    let mut reference_sources_absent = 0usize;
    for row in input.rows {
        if let Some(page) = &row.page {
            let relevant = page.html() || row.references.data().is_some();
            if relevant {
                match &row.references {
                    CollectorValue::Observed(data) if data.complete => {
                        reference_sources_observed += 1;
                    }
                    CollectorValue::Observed(_) | CollectorValue::Partial(_) => {
                        reference_sources_partial += 1;
                    }
                    CollectorValue::Unavailable { .. } => reference_sources_unavailable += 1,
                    CollectorValue::Absent | CollectorValue::Invalid { .. } => {
                        reference_sources_absent += 1;
                    }
                }
            }
        }
        let Some(page) = &row.page else {
            continue;
        };
        for relationship in page
            .relationships
            .iter()
            .filter(|relationship| relationship.resource)
        {
            if let Some(state) = accumulators.get_mut(&relationship.url) {
                add_referrer(
                    state,
                    &mut total_referrers,
                    relationship_referrer(row, relationship),
                );
            }
        }
        let Some(references) = row.references.data() else {
            continue;
        };
        let base = Url::parse(&row.url).ok();
        for occurrence in &references.occurrences {
            if let (Some(base), Some(raw_src)) = (&base, &occurrence.raw_src)
                && let Some(target) = normalize(base, raw_src)
                && let Some(state) = accumulators.get_mut(target.as_str())
            {
                let attribute = if occurrence.element.eq_ignore_ascii_case("link") {
                    "href"
                } else {
                    "src"
                };
                add_referrer(
                    state,
                    &mut total_referrers,
                    reference_referrer(row, references, occurrence, raw_src, attribute, None),
                );
            }
            if let (Some(base), Some(raw_srcset)) = (&base, &occurrence.raw_srcset) {
                for (candidate, descriptor) in crate::extract::srcset(raw_srcset) {
                    if let Some(target) = normalize(base, &candidate)
                        && let Some(state) = accumulators.get_mut(target.as_str())
                    {
                        add_referrer(
                            state,
                            &mut total_referrers,
                            reference_referrer(
                                row,
                                references,
                                occurrence,
                                &candidate,
                                "srcset",
                                Some(&descriptor),
                            ),
                        );
                    }
                }
            }
        }
    }

    let reference_source_total = reference_sources_observed
        .saturating_add(reference_sources_partial)
        .saturating_add(reference_sources_unavailable)
        .saturating_add(reference_sources_absent);
    let has_html_source = input
        .rows
        .iter()
        .any(|row| !row.is_resource && row.page.as_ref().is_some_and(Page::html));
    let referrer_coverage = if affected_coverage_partial
        || reference_sources_partial > 0
        || reference_sources_unavailable > 0
        || reference_sources_absent > 0
    {
        "partial"
    } else if reference_source_total == 0 {
        "unknown"
    } else {
        "bounded"
    };
    let affected_pages_coverage = if affected_coverage_partial {
        "partial"
    } else if has_html_source {
        "bounded"
    } else {
        "unknown"
    };

    let mut total_affected = 0usize;
    for (url, pages) in &affected {
        let Some(state) = accumulators.get_mut(url) else {
            continue;
        };
        for page in pages {
            if state.affected.len() >= MAX_AFFECTED_PER_RESOURCE
                || total_affected >= MAX_AFFECTED_TOTAL
            {
                state.omitted_affected = state.omitted_affected.saturating_add(1);
                continue;
            }
            total_affected += 1;
            state.affected.push(page.clone());
        }
    }

    let mut groups: BTreeMap<(Option<String>, Option<String>), ByteAccum> = BTreeMap::new();
    let mut counted_resources = 0usize;
    let mut total_bytes = 0u64;
    let mut unknown_bytes = 0usize;
    let mut excluded = 0usize;
    let mut hashes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for state in accumulators.values() {
        let row = &input.rows[state.row_index];
        if let Some(value) = row.resource.data() {
            hashes
                .entry(value.body_hash.clone())
                .or_default()
                .insert(row.url.clone());
        }
    }
    let mut resources = Vec::new();

    for (url, state) in &mut accumulators {
        let row = &input.rows[state.row_index];
        let response = response_state(row);
        let measurement = measurement_state(&row.resource);
        let data = row.resource.data();
        let bytes = resource_bytes(data, &measurement);
        let mime = mime_observation(data, &measurement);
        let compression = compression_observation(data, &measurement);
        let cache = cache_observation(data, &measurement);
        let body_sha256 = match data {
            Some(value) => field(
                Some(value.body_hash.clone()),
                "measured_decoded_body",
                "transport_body",
            ),
            None => unknown(
                "unknown",
                "transport_body",
                format!("measurement_{}", reason_text(measurement.reason.as_deref())),
            ),
        };
        state.counted = false;
        state.exclusion_reason = match response.http_success {
            Some(true) if data.is_some() => {
                state.counted = true;
                None
            }
            Some(false) => Some("unsuccessful_http_status"),
            Some(true) => Some("resource_measurement_unavailable"),
            None if response.state == "not_fetched" => Some("resource_not_fetched"),
            None if response.state == "blocked_by_robots" => Some("blocked_by_robots"),
            None if response.state == "page_evidence_omitted" => {
                Some("response_page_evidence_omitted")
            }
            None => Some("response_status_unavailable"),
        };
        let origin = url_origin(url);
        let mime_key = data
            .and_then(|value| value.content_type.as_deref())
            .or_else(|| {
                row.page
                    .as_ref()
                    .map(|page| page.content_type.as_str())
                    .filter(|value| !value.is_empty())
            })
            .and_then(mime_type);
        let group = groups.entry((origin, mime_key)).or_default();
        if state.counted {
            counted_resources += 1;
            let value = data.map_or(0, |measurement| measurement.decoded_body_bytes);
            match total_bytes.checked_add(value) {
                Some(sum) => total_bytes = sum,
                None => group.complete = false,
            }
            match group.bytes.checked_add(value) {
                Some(sum) => group.bytes = sum,
                None => group.complete = false,
            }
            group.counted += 1;
            if row.resource.is_partial() {
                group.complete = false;
            }
        } else {
            excluded += 1;
            group.excluded += 1;
            // An excluded member is not in the group sum, so the group can only
            // be a lower bound. Keep group and aggregate completeness aligned.
            group.complete = false;
        }
        if data.is_none() {
            unknown_bytes += 1;
            group.unknown += 1;
            group.complete = false;
        }
        let duplicate_group_sha256 = data.and_then(|value| {
            hashes
                .get(&value.body_hash)
                .filter(|urls| urls.len() > 1)
                .map(|_| value.body_hash.clone())
        });
        resources.push(ResourceImpactEntry {
            url: url_value(url),
            response,
            measurement,
            bytes,
            body_sha256,
            mime,
            compression,
            cache,
            referrers: ReferrerSummary {
                coverage: referrer_coverage,
                observed_referrers: state.referrers.len(),
                omitted_referrers: state.omitted_referrers,
                items: std::mem::take(&mut state.referrers),
            },
            affected_pages: AffectedPages {
                semantics: "static_resource_dependency_graph",
                coverage: affected_pages_coverage,
                pages: state.affected.iter().map(|page| url_value(page)).collect(),
                omitted_pages: state.omitted_affected,
            },
            duplicate_group_sha256,
            counted_in_byte_totals: state.counted,
            exclusion_reason: state.exclusion_reason,
        });
    }

    let mut duplicates: Vec<DuplicateGroup> = hashes
        .into_iter()
        .filter(|(_, urls)| urls.len() > 1)
        .map(|(hash, urls)| DuplicateGroup {
            decoded_body_hash: hash,
            urls: urls.iter().map(|url| url_value(url)).collect(),
            semantics: "measured_decoded_body_equality",
        })
        .collect();
    duplicates.sort_by(|a, b| a.decoded_body_hash.cmp(&b.decoded_body_hash));

    let bytes_complete = !input.input_truncated
        && unknown_bytes == 0
        && excluded == 0
        && groups.values().all(|group| group.complete);
    let groups: Vec<ByteGroup> = groups
        .into_iter()
        .map(|((origin, mime), group)| ByteGroup {
            origin,
            mime,
            counted_resources: group.counted,
            decoded_body_bytes: (group.counted > 0).then_some(group.bytes),
            unknown_decoded_bytes_resources: group.unknown,
            excluded_resources: group.excluded,
            complete: group.complete && !input.input_truncated,
        })
        .collect();

    let requested_resources = accumulators.len();
    let measured_resources = accumulators
        .values()
        .filter(|state| input.rows[state.row_index].resource.data().is_some())
        .count();
    let unsuccessful_resources = accumulators
        .values()
        .filter(|state| {
            input.rows[state.row_index]
                .page
                .as_ref()
                .and_then(|page| page.status)
                .is_some_and(|status| !(200..300).contains(&status))
        })
        .count();
    let not_fetched_resources = accumulators
        .values()
        .filter(|state| !input.rows[state.row_index].fetched)
        .count();
    ResourceImpact {
        coverage: ImpactCoverage {
            requested_resources,
            measured_resources,
            unknown_measurement_resources: requested_resources.saturating_sub(measured_resources),
            unsuccessful_resources,
            not_fetched_resources,
            reference_sources_observed,
            reference_sources_partial,
            reference_sources_unavailable,
            reference_sources_absent,
            affected_pages_coverage,
            referrer_coverage,
            bytes_complete,
            input_truncated: input.input_truncated,
            discovery_coverage: "unknown",
        },
        bytes: ByteSummary {
            semantics: ByteSemantics {
                decoded_body: "measured_transport_decoded_response_body",
                declared_content_length: "header_estimate_not_transfer",
                transfer: "unknown_not_measured",
            },
            counted_resources,
            decoded_body_bytes: (counted_resources > 0).then_some(total_bytes),
            unknown_decoded_bytes_resources: unknown_bytes,
            excluded_resources: excluded,
            complete: bytes_complete,
            groups,
        },
        resources,
        duplicates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn measurement(headers: &BTreeMap<String, String>, body: &[u8]) -> SavedResourceMeasurement {
        let value = crate::resource_evidence::resource("https://example.test/a.png", headers, body);
        serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap()
    }

    fn row(url: &str, is_resource: bool, page: Page) -> SavedPageRow {
        SavedPageRow {
            url: url.into(),
            is_resource,
            fetched: true,
            page: Some(page),
            page_omission: None,
            resource: CollectorValue::Absent,
            references: CollectorValue::Absent,
        }
    }

    #[test]
    fn pr43_and_pr52_payloads_parse_and_missing_bytes_stay_unknown() {
        let headers = BTreeMap::from([
            ("content-length".into(), "999".into()),
            ("content-type".into(), "image/gif".into()),
            ("content-encoding".into(), "gzip".into()),
            ("cache-control".into(), "max-age=60".into()),
            ("etag".into(), "\"v1\"".into()),
            ("last-modified".into(), "yesterday".into()),
        ]);
        let gif = b"GIF89a\x02\x00\x03\x00";
        let parsed = measurement(&headers, gif);
        assert_eq!(parsed.decoded_body_bytes, 10);
        assert_eq!(parsed.declared_content_length, Some(999));
        assert_eq!(parsed.detected_image_format.as_deref(), Some("gif"));
        assert_eq!(parsed.image_dimensions, Some((2, 3)));
        let references: SavedResourceReferences = serde_json::from_value(
            serde_json::to_value(crate::resource_evidence::references(
                "https://example.test/",
                include_str!("../fixtures/s08/references.html"),
            ))
            .unwrap(),
        )
        .unwrap();
        assert!(references.complete);
        assert_eq!(references.occurrences.len(), 4);

        let mut html = row(
            "https://example.test/",
            false,
            Page {
                url: "https://example.test/".into(),
                status: Some(200),
                content_type: "text/html".into(),
                relationships: vec![Relationship {
                    kind: "stylesheet".into(),
                    url: "https://example.test/a.png".into(),
                    descriptor: "screen".into(),
                    resource: true,
                    ..Relationship::default()
                }],
                ..Page::default()
            },
        );
        html.references = CollectorValue::Observed(references);
        let mut resource = row(
            "https://example.test/a.png",
            true,
            Page {
                url: "https://example.test/a.png".into(),
                status: Some(200),
                content_type: "image/gif".into(),
                is_resource: true,
                ..Page::default()
            },
        );
        resource.resource = CollectorValue::Observed(parsed);
        let missing = row(
            "https://example.test/b.png",
            true,
            Page {
                url: "https://example.test/b.png".into(),
                status: Some(200),
                content_type: "image/png".into(),
                is_resource: true,
                ..Page::default()
            },
        );
        let impact = analyze(&ImpactInput {
            rows: &[html, resource, missing],
            input_truncated: false,
        });
        assert_eq!(impact.bytes.decoded_body_bytes, Some(10));
        assert_eq!(impact.bytes.unknown_decoded_bytes_resources, 1);
        assert!(!impact.bytes.complete);
        let missing = impact
            .resources
            .iter()
            .find(|value| value.url.url.as_deref() == Some("https://example.test/b.png"))
            .unwrap();
        assert!(missing.bytes.decoded_body.value.is_none());
        assert_eq!(missing.bytes.decoded_body.semantics, "unknown");
        assert!(!missing.counted_in_byte_totals);
        let measured = impact
            .resources
            .iter()
            .find(|value| value.url.url.as_deref() == Some("https://example.test/a.png"))
            .unwrap();
        assert_eq!(measured.bytes.declared_content_length.value, Some(999));
        assert_eq!(
            measured.bytes.declared_content_length.semantics,
            "estimated"
        );
        assert!(measured.bytes.transfer.value.is_none());
        assert_eq!(measured.referrers.observed_referrers, 1);
        assert_eq!(measured.affected_pages.pages.len(), 1);
        assert_eq!(
            measured.mime.detected_signature.value.as_deref(),
            Some("gif")
        );
        assert_eq!(measured.cache.etag.value.as_deref(), Some("\"v1\""));
    }

    #[test]
    fn absent_measurements_keep_null_totals_and_do_not_count_zero() {
        let missing = row(
            "https://example.test/missing.png",
            true,
            Page {
                url: "https://example.test/missing.png".into(),
                status: Some(200),
                content_type: "image/png".into(),
                is_resource: true,
                ..Page::default()
            },
        );
        let impact = analyze(&ImpactInput {
            rows: &[missing],
            input_truncated: false,
        });
        assert!(impact.bytes.decoded_body_bytes.is_none());
        assert_eq!(impact.bytes.counted_resources, 0);
        assert_eq!(impact.bytes.unknown_decoded_bytes_resources, 1);
        assert_eq!(impact.bytes.groups.len(), 1);
        assert_eq!(impact.bytes.groups[0].decoded_body_bytes, None);
        assert_eq!(impact.bytes.groups[0].unknown_decoded_bytes_resources, 1);
        assert!(!impact.bytes.groups[0].complete);
        assert!(!impact.resources[0].counted_in_byte_totals);
        assert!(impact.resources[0].bytes.decoded_body.value.is_none());
    }

    #[test]
    fn srcset_candidates_duplicate_hashes_and_partial_sources_are_explicit() {
        let references = SavedResourceReferences {
            source_url: "https://example.test/".into(),
            occurrences: vec![SavedReferenceOccurrence {
                element: "img".into(),
                raw_src: None,
                raw_rel: None,
                raw_srcset: Some("/a.png 1x, /b.png 2x".into()),
                media: None,
                declared_type: Some("image/png".into()),
                as_attribute: None,
                crossorigin: None,
                sizes: None,
                loading: Some("lazy".into()),
                fetchpriority: None,
                width: None,
                height: None,
            }],
            complete: false,
        };
        let mut html = row(
            "https://example.test/",
            false,
            Page {
                url: "https://example.test/".into(),
                status: Some(200),
                content_type: "text/html".into(),
                ..Page::default()
            },
        );
        html.references = CollectorValue::Partial(references);
        let asset = |url: &str, body: &[u8]| {
            let measured = crate::resource_evidence::resource(
                url,
                &BTreeMap::from([("content-type".into(), "image/png".into())]),
                body,
            );
            let mut value = row(
                url,
                true,
                Page {
                    url: url.into(),
                    status: Some(200),
                    content_type: "image/png".into(),
                    is_resource: true,
                    ..Page::default()
                },
            );
            value.resource = CollectorValue::Observed(
                serde_json::from_value(serde_json::to_value(measured).unwrap()).unwrap(),
            );
            value
        };
        let a = asset("https://example.test/a.png", b"same");
        let b = asset("https://example.test/b.png", b"same");
        let impact = analyze(&ImpactInput {
            rows: &[html, a, b],
            input_truncated: false,
        });
        for entry in &impact.resources {
            assert_eq!(entry.referrers.observed_referrers, 1);
            assert_eq!(
                entry.referrers.items[0].attribute.as_deref(),
                Some("srcset")
            );
            assert_eq!(entry.referrers.coverage, "partial");
        }
        assert_eq!(impact.duplicates.len(), 1);
        assert_eq!(impact.duplicates[0].urls.len(), 2);
        assert_eq!(impact.bytes.decoded_body_bytes, Some(8));
    }

    fn measured(url: &str, status: Option<u16>, mime: &str, body: &[u8]) -> SavedPageRow {
        let measured = crate::resource_evidence::resource(
            url,
            &BTreeMap::from([("content-type".into(), mime.into())]),
            body,
        );
        let mut value = row(
            url,
            true,
            Page {
                url: url.into(),
                status,
                content_type: mime.into(),
                is_resource: true,
                ..Page::default()
            },
        );
        value.resource = CollectorValue::Observed(
            serde_json::from_value(serde_json::to_value(measured).unwrap()).unwrap(),
        );
        value
    }

    #[test]
    fn group_with_successful_and_excluded_members_is_incomplete() {
        let ok = measured(
            "https://example.test/a.gif",
            Some(200),
            "image/gif",
            b"GIF89a....",
        );
        let gone = measured(
            "https://example.test/gone.gif",
            Some(404),
            "image/gif",
            b"notfound",
        );
        let sheet = measured(
            "https://example.test/a.css",
            Some(200),
            "text/css",
            b"body{}",
        );
        let impact = analyze(&ImpactInput {
            rows: &[ok, gone, sheet],
            input_truncated: false,
        });
        assert_eq!(impact.bytes.counted_resources, 2);
        assert_eq!(impact.bytes.excluded_resources, 1);
        assert_eq!(impact.bytes.decoded_body_bytes, Some(16));
        assert_eq!(impact.bytes.groups.len(), 2);

        let gif = impact
            .bytes
            .groups
            .iter()
            .find(|group| group.mime.as_deref() == Some("image/gif"))
            .unwrap();
        assert_eq!(gif.counted_resources, 1);
        assert_eq!(gif.excluded_resources, 1);
        // Only the counted member contributes, so the group value is a lower bound.
        assert_eq!(gif.decoded_body_bytes, Some(10));
        assert!(!gif.complete);

        // A fully counted group stays complete even though the totals are not.
        let css = impact
            .bytes
            .groups
            .iter()
            .find(|group| group.mime.as_deref() == Some("text/css"))
            .unwrap();
        assert_eq!(css.counted_resources, 1);
        assert_eq!(css.excluded_resources, 0);
        assert_eq!(css.decoded_body_bytes, Some(6));
        assert!(css.complete);
        assert!(!impact.bytes.complete);
    }

    #[test]
    fn all_excluded_and_missing_status_groups_stay_null_and_incomplete() {
        let mut not_fetched = measured(
            "https://example.test/never.gif",
            Some(200),
            "image/gif",
            b"unused",
        );
        not_fetched.fetched = false;
        let mut omitted = measured(
            "https://example.test/omitted.gif",
            Some(200),
            "image/gif",
            b"unused",
        );
        omitted.page = None;
        let mut blocked = measured(
            "https://example.test/blocked.gif",
            Some(200),
            "image/gif",
            b"unused",
        );
        blocked.page.as_mut().unwrap().blocked_by_robots = true;
        let impact = analyze(&ImpactInput {
            rows: &[
                measured(
                    "https://example.test/gone.gif",
                    Some(404),
                    "image/gif",
                    b"notfound",
                ),
                not_fetched,
                omitted,
                measured(
                    "https://example.test/nostatus.gif",
                    None,
                    "image/gif",
                    b"unused",
                ),
                blocked,
            ],
            input_truncated: false,
        });
        assert_eq!(impact.bytes.counted_resources, 0);
        assert_eq!(impact.bytes.excluded_resources, 5);
        assert_eq!(impact.bytes.unknown_decoded_bytes_resources, 0);
        assert!(impact.bytes.decoded_body_bytes.is_none());
        assert!(!impact.bytes.complete);
        assert_eq!(impact.bytes.groups.len(), 1);
        let group = &impact.bytes.groups[0];
        assert_eq!(group.counted_resources, 0);
        assert_eq!(group.excluded_resources, 5);
        assert_eq!(group.decoded_body_bytes, None);
        assert!(!group.complete);
        let reasons: BTreeSet<&str> = impact
            .resources
            .iter()
            .map(|entry| entry.exclusion_reason.unwrap())
            .collect();
        assert_eq!(
            reasons,
            BTreeSet::from([
                "blocked_by_robots",
                "resource_not_fetched",
                "response_page_evidence_omitted",
                "response_status_unavailable",
                "unsuccessful_http_status",
            ])
        );
        assert!(
            impact
                .resources
                .iter()
                .all(|entry| !entry.counted_in_byte_totals)
        );
    }
}
