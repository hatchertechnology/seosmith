//! Named job-failure codes (seosmith#64, WP8b-1).
//!
//! `jobs.error` used to say `crawl_internal_error` for every internal failure,
//! so the 14d run (1,753 URLs, fetched in full, then failed) could not be told
//! apart from a crashed crawl. Every generation check now carries a
//! [`JobErrorCode`]; the worker stores and logs that code.
//!
//! The code vocabulary is the stored `jobs.error` text and is also read by the
//! status API and `operations`; add codes, never rename them.
use std::fmt;

/// The fixed set of `jobs.error` codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobErrorCode {
    /// Fallback for a crawl future that returned `Err`.
    CrawlInternalError,
    /// A whole-report byte or depth bound (16 MiB report, HTP byte cap).
    ReportByteLimit,
    /// The S14 manifest is over its byte limit (262,144 B in `s14-wire-1`).
    PolicyManifestLimit,
    /// `evidence_contract` is over its extension limit.
    EvidenceExtensionLimit,
    /// `policy_guidance` or its canonical policy is over its extension limit.
    PolicyExtensionLimit,
    /// A recomputed population (findings, edges) differs from the retained one.
    PopulationMismatch,
    /// Any other generation check, including unclassified generation errors.
    GenerationInvariant,
    /// Persisting the finished report failed.
    StoreError,
    /// The operations adapter ran out of time.
    AdapterDeadlineExceeded,
}

impl JobErrorCode {
    /// The text stored in `jobs.error`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CrawlInternalError => "crawl_internal_error",
            Self::ReportByteLimit => "report_byte_limit",
            Self::PolicyManifestLimit => "policy_manifest_limit",
            Self::EvidenceExtensionLimit => "evidence_extension_limit",
            Self::PolicyExtensionLimit => "policy_extension_limit",
            Self::PopulationMismatch => "population_mismatch",
            Self::GenerationInvariant => "generation_invariant",
            Self::StoreError => "store_error",
            Self::AdapterDeadlineExceeded => "adapter_deadline_exceeded",
        }
    }

    /// Inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::CrawlInternalError,
            Self::ReportByteLimit,
            Self::PolicyManifestLimit,
            Self::EvidenceExtensionLimit,
            Self::PolicyExtensionLimit,
            Self::PopulationMismatch,
            Self::GenerationInvariant,
            Self::StoreError,
            Self::AdapterDeadlineExceeded,
        ]
        .into_iter()
        .find(|code| code.as_str() == text)
    }
}

/// A generation failure with a stable code and a fixed, request-free detail.
///
/// `detail` is a `&'static str` on purpose: it can never carry a URL, a body or
/// a token, so it is safe to log (the no-diagnostics rule in `main.rs`).
/// `Display` prints the detail only, which keeps the pre-existing messages
/// that tests and logs match on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GenerationError {
    pub code: JobErrorCode,
    pub detail: &'static str,
}

impl GenerationError {
    #[must_use]
    pub const fn new(code: JobErrorCode, detail: &'static str) -> Self {
        Self { code, detail }
    }
}

impl fmt::Display for GenerationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.detail)
    }
}

impl std::error::Error for GenerationError {}

/// Recovers the typed error from anywhere in an `anyhow` chain. An error that
/// was never typed is reported as [`JobErrorCode::GenerationInvariant`] with a
/// fixed detail: it happened inside generation, and its message may hold
/// request data, so it is not forwarded.
impl From<anyhow::Error> for GenerationError {
    fn from(error: anyhow::Error) -> Self {
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<Self>().copied())
            .unwrap_or(Self::new(
                JobErrorCode::GenerationInvariant,
                "unclassified generation error",
            ))
    }
}

/// `ensure!` with a code. Works in functions returning either
/// `anyhow::Result` or `Result<_, GenerationError>`.
pub const fn ensure_code(
    condition: bool,
    code: JobErrorCode,
    detail: &'static str,
) -> Result<(), GenerationError> {
    if condition {
        Ok(())
    } else {
        Err(GenerationError::new(code, detail))
    }
}

/// A [`JobErrorCode::GenerationInvariant`] failure with a fixed detail.
#[must_use]
pub const fn invariant(detail: &'static str) -> GenerationError {
    GenerationError::new(JobErrorCode::GenerationInvariant, detail)
}

/// `ensure!` for generation checks that are plain invariants. The message must
/// be a literal, so it can never carry request data.
macro_rules! ensure_invariant {
    ($condition:expr, $detail:literal $(,)?) => {
        $crate::generation_error::ensure_code(
            $condition,
            $crate::generation_error::JobErrorCode::GenerationInvariant,
            $detail,
        )?
    };
}
pub(crate) use ensure_invariant;

/// `ensure!` with an explicit code, for the checks that name a limit.
macro_rules! ensure_coded {
    ($code:expr, $condition:expr, $detail:literal $(,)?) => {
        $crate::generation_error::ensure_code($condition, $code, $detail)?
    };
}
pub(crate) use ensure_coded;

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [(JobErrorCode, &str); 9] = [
        (JobErrorCode::CrawlInternalError, "crawl_internal_error"),
        (JobErrorCode::ReportByteLimit, "report_byte_limit"),
        (JobErrorCode::PolicyManifestLimit, "policy_manifest_limit"),
        (
            JobErrorCode::EvidenceExtensionLimit,
            "evidence_extension_limit",
        ),
        (JobErrorCode::PolicyExtensionLimit, "policy_extension_limit"),
        (JobErrorCode::PopulationMismatch, "population_mismatch"),
        (JobErrorCode::GenerationInvariant, "generation_invariant"),
        (JobErrorCode::StoreError, "store_error"),
        (
            JobErrorCode::AdapterDeadlineExceeded,
            "adapter_deadline_exceeded",
        ),
    ];

    #[test]
    fn stored_text_is_the_design_vocabulary_and_round_trips() {
        for (code, text) in ALL {
            assert_eq!(code.as_str(), text);
            assert_eq!(JobErrorCode::parse(text), Some(code));
        }
        assert_eq!(JobErrorCode::parse("anything_else"), None);
    }

    #[test]
    fn display_is_the_detail_so_existing_messages_keep_matching() {
        let e = GenerationError::new(JobErrorCode::PolicyManifestLimit, "manifest byte limit");
        assert_eq!(e.to_string(), "manifest byte limit");
        assert_eq!(e.code, JobErrorCode::PolicyManifestLimit);
    }

    #[test]
    fn ensure_code_passes_and_fails_with_the_code() {
        assert!(ensure_code(true, JobErrorCode::StoreError, "x").is_ok());
        let e = ensure_code(false, JobErrorCode::ReportByteLimit, "too big").unwrap_err();
        assert_eq!(
            (e.code, e.detail),
            (JobErrorCode::ReportByteLimit, "too big")
        );
    }

    #[test]
    fn a_typed_error_survives_an_anyhow_round_trip() {
        let typed =
            GenerationError::new(JobErrorCode::PopulationMismatch, "edge population differs");
        let wrapped: anyhow::Error = typed.into();
        let back = GenerationError::from(wrapped);
        assert_eq!(back, typed);
        // Context added on the way up must not hide the code.
        let wrapped = anyhow::Error::new(typed).context("while generating");
        assert_eq!(
            GenerationError::from(wrapped).code,
            JobErrorCode::PopulationMismatch
        );
    }

    #[test]
    fn an_untyped_error_becomes_generation_invariant_with_a_fixed_detail() {
        let e = GenerationError::from(anyhow::anyhow!("secret request body https://x.example"));
        assert_eq!(e.code, JobErrorCode::GenerationInvariant);
        assert_eq!(e.detail, "unclassified generation error");
        assert!(!e.to_string().contains("secret"));
    }
}
