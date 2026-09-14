use super::super::ConversionCandidate;

/// Stable cost order. Equal costs keep the order search already produced.
pub(in crate::conversion) fn sort_by_cost(candidates: &mut [ConversionCandidate]) {
    candidates.sort_by_key(|candidate| candidate.cost);
}
