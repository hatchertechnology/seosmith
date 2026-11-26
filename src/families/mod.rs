//! WP8b-2: evidence families (S02-S08) for the worker's wire-2 reports.
pub mod types;

/// Which kind of job an analyzer entry point may read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobSource {
    /// The CLI readers: a terminal local-only job in an immutable snapshot.
    SavedLocal,
    /// The worker's own standard job, read through the live store.
    LiveStandard,
}

impl JobSource {
    pub(crate) fn accepts(self, state: &str, mode: Option<&str>) -> bool {
        match self {
            JobSource::SavedLocal => state == "local_only" && mode == Some("local"),
            JobSource::LiveStandard => mode == Some("standard"),
        }
    }
    pub(crate) fn completion(self, finalization: Option<&str>) -> &'static str {
        match (self, finalization) {
            (JobSource::LiveStandard, _) => "live",
            (_, Some("completed")) => "completed",
            (_, Some("interrupted")) => "interrupted",
            _ => "unknown",
        }
    }
}
