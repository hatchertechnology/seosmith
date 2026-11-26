pub mod api;
pub mod audit;
pub mod collection_evidence;
pub mod crawler;
pub mod delivery;
pub mod fetch;
pub mod index_evidence;
pub mod integrity_evidence;
pub mod model;
pub mod parse;
pub mod robots_evidence;
pub mod sitemap_evidence;
pub mod store;

pub mod extract;

pub mod families;
pub mod resource_impact_cli;
pub mod saved_graph;
pub mod saved_index_integrity;

pub mod contract;

pub mod policy;

pub mod generation_error;
pub mod operations;
pub mod policy_candidate;

pub mod structured_validation;

#[cfg(all(feature = "local-e2e", not(debug_assertions)))]
compile_error!("the local-e2e feature is test-only and cannot be built without debug assertions");

pub mod finding_coverage_prototype;
pub mod policy_wire_prototype;

/// Experimental offline link graph analysis.
pub mod graph_evidence;

/// Experimental offline static resource observations.
pub mod resource_evidence;

/// Bounded saved-checkpoint resource delivery and affected-page analysis.
pub mod resource_impact;

/// Per-page capture collectors (also read by `examples/measure_capture.rs`).
pub mod local_checkpoint;

pub mod s06_batch;
/// Experimental versioned content detector.
pub mod s06_detector;
