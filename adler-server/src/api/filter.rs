use adler_core::{Site, SiteFilter};

use super::dto::StartScanRequest;

/// Apply per-scan name/tag/popularity filters to a catalog slice.
///
/// Mirrors [`adler_core::Registry::filter`] semantics but works on a
/// `&[Site]` so it can compose with the catalog already filtered at
/// server startup.
pub(super) fn filter_catalog(catalog: &[Site], req: &StartScanRequest) -> Vec<Site> {
    scan_filter(req).apply(catalog)
}

/// Apply the same request filter but return only disabled entries for
/// diagnostics.
pub(super) fn disabled_matches(catalog: &[Site], req: &StartScanRequest) -> Vec<Site> {
    scan_filter(req)
        .apply_including_disabled(catalog)
        .into_iter()
        .filter(|s| s.disabled)
        .collect()
}

fn scan_filter(req: &StartScanRequest) -> SiteFilter {
    SiteFilter {
        include: req.only.clone(),
        exclude: req.exclude.clone(),
        tags: req.tag.clone(),
        exclude_tags: req.exclude_tag.clone(),
        include_nsfw: req.nsfw,
        top: req.top,
    }
}
