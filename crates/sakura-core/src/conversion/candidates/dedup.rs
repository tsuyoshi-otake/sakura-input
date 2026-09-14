use super::ConversionCandidate;

pub(in crate::conversion) fn has_same_surface(
    candidates: &[ConversionCandidate],
    text: &str,
) -> bool {
    candidates.iter().any(|existing| existing.text() == text)
}
