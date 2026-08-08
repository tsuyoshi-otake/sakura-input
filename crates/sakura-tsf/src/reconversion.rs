//! COM candidate objects returned by `ITfFnReconversion::GetReconversion`.
//!
//! These objects are snapshots. They never retain an engine connection or a
//! document range, so a host can enumerate them after the original request has
//! returned without keeping mutable IME state alive.

use std::cell::Cell;
use std::sync::Arc;

use sakura_proto::CandidateList;
use windows::Win32::Foundation::{E_INVALIDARG, E_POINTER, S_FALSE};
use windows::Win32::UI::TextServices::{
    IEnumTfCandidates, IEnumTfCandidates_Impl, ITfCandidateList, ITfCandidateList_Impl,
    ITfCandidateString, ITfCandidateString_Impl, TfCandidateResult, CAND_CANCELED, CAND_FINALIZED,
    CAND_SELECTED,
};
use windows_core::{implement, Error, IUnknownImpl, Result, BSTR};

type CandidateSnapshot = Arc<[Arc<str>]>;

#[implement(ITfCandidateString)]
#[derive(Debug)]
struct CandidateString {
    index: u32,
    text: Arc<str>,
}

impl CandidateString {
    fn new(index: usize, text: Arc<str>) -> Result<Self> {
        Ok(Self {
            index: u32::try_from(index).map_err(|_| Error::from_hresult(E_INVALIDARG))?,
            text,
        })
    }
}

impl ITfCandidateString_Impl for CandidateString_Impl {
    fn GetString(&self) -> Result<BSTR> {
        Ok(BSTR::from(self.get_impl().text.as_ref()))
    }

    fn GetIndex(&self) -> Result<u32> {
        Ok(self.get_impl().index)
    }
}

#[implement(ITfCandidateList)]
#[derive(Debug)]
struct ReconversionCandidateList {
    candidates: CandidateSnapshot,
}

impl ReconversionCandidateList {
    fn new(candidates: CandidateSnapshot) -> Self {
        Self { candidates }
    }
}

impl ITfCandidateList_Impl for ReconversionCandidateList_Impl {
    fn EnumCandidates(&self) -> Result<IEnumTfCandidates> {
        Ok(CandidateEnumerator::new(Arc::clone(&self.get_impl().candidates), 0).into())
    }

    fn GetCandidate(&self, nindex: u32) -> Result<ITfCandidateString> {
        candidate_at(&self.get_impl().candidates, nindex as usize)
    }

    fn GetCandidateNum(&self) -> Result<u32> {
        u32::try_from(self.get_impl().candidates.len())
            .map_err(|_| Error::from_hresult(E_INVALIDARG))
    }

    fn SetResult(&self, nindex: u32, imcr: TfCandidateResult) -> Result<()> {
        if nindex as usize >= self.get_impl().candidates.len()
            || ![CAND_FINALIZED, CAND_SELECTED, CAND_CANCELED].contains(&imcr)
        {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        // The actual reconversion UI is driven through Sakura's ordinary
        // candidate flow. This snapshot only reports that the host consumed a
        // candidate result; it owns no session state that could be mutated.
        Ok(())
    }
}

#[implement(IEnumTfCandidates)]
#[derive(Debug)]
struct CandidateEnumerator {
    candidates: CandidateSnapshot,
    position: Cell<usize>,
}

impl CandidateEnumerator {
    fn new(candidates: CandidateSnapshot, position: usize) -> Self {
        Self {
            candidates,
            position: Cell::new(position),
        }
    }
}

impl IEnumTfCandidates_Impl for CandidateEnumerator_Impl {
    fn Clone(&self) -> Result<IEnumTfCandidates> {
        let state = self.get_impl();
        Ok(CandidateEnumerator::new(Arc::clone(&state.candidates), state.position.get()).into())
    }

    fn Next(
        &self,
        ulcount: u32,
        ppcand: *mut Option<ITfCandidateString>,
        pcfetched: *mut u32,
    ) -> Result<()> {
        if ppcand.is_null() && ulcount != 0 {
            return Err(Error::from_hresult(E_POINTER));
        }
        if pcfetched.is_null() && ulcount != 1 {
            return Err(Error::from_hresult(E_POINTER));
        }

        let state = self.get_impl();
        let start = state.position.get();
        let mut fetched = 0usize;
        while (fetched as u32) < ulcount && start + fetched < state.candidates.len() {
            let candidate = candidate_at(&state.candidates, start + fetched)?;
            // SAFETY: the caller provides `ulcount` writable interface slots;
            // `fetched` is bounded by that count. `write` avoids releasing an
            // uninitialized bit pattern in the destination slot.
            unsafe { ppcand.add(fetched).write(Some(candidate)) };
            fetched += 1;
        }
        state.position.set(start + fetched);
        if !pcfetched.is_null() {
            // SAFETY: non-null checked above (or optional for a one-item call)
            // and COM guarantees a writable count value.
            unsafe { pcfetched.write(fetched as u32) };
        }

        if fetched as u32 == ulcount {
            Ok(())
        } else {
            // Preserve COM's partial-enumeration status through the generated
            // `Result` to HRESULT adapter.
            Err(Error::from_hresult(S_FALSE))
        }
    }

    fn Reset(&self) -> Result<()> {
        self.get_impl().position.set(0);
        Ok(())
    }

    fn Skip(&self, ulcount: u32) -> Result<()> {
        let state = self.get_impl();
        let position = state.position.get();
        let requested = position.saturating_add(ulcount as usize);
        state.position.set(requested.min(state.candidates.len()));
        if requested <= state.candidates.len() {
            Ok(())
        } else {
            Err(Error::from_hresult(S_FALSE))
        }
    }
}

fn candidate_at(candidates: &CandidateSnapshot, index: usize) -> Result<ITfCandidateString> {
    let text = candidates
        .get(index)
        .cloned()
        .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
    Ok(CandidateString::new(index, text)?.into())
}

/// Copies an engine result into a self-contained TSF candidate list.
pub fn candidate_list(candidates: &CandidateList) -> Result<ITfCandidateList> {
    if candidates.items.is_empty() {
        return Err(Error::from_hresult(E_INVALIDARG));
    }
    let snapshot: CandidateSnapshot = candidates
        .items
        .iter()
        .map(|candidate| Arc::<str>::from(candidate.text.as_str()))
        .collect::<Vec<_>>()
        .into();
    Ok(ReconversionCandidateList::new(snapshot).into())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use sakura_proto::types::CandidatePresentation;
    use sakura_proto::{Candidate, CandidateKind};

    fn fixture() -> ITfCandidateList {
        candidate_list(&CandidateList {
            kind: CandidateKind::Conversion,
            presentation: CandidatePresentation::Compact,
            selected: 0,
            page_size: 9,
            items: vec![
                Candidate {
                    text: "仮名".to_owned(),
                    annotation: "IT用語".to_owned(),
                },
                Candidate {
                    text: "加奈".to_owned(),
                    annotation: "人名".to_owned(),
                },
            ],
        })
        .expect("candidate list")
    }

    #[test]
    fn list_exposes_indexed_strings_and_a_cloneable_enumerator() {
        let list = fixture();
        // SAFETY: these are calls on locally created, live COM objects and all
        // output buffers have their declared length.
        unsafe {
            assert_eq!(list.GetCandidateNum().expect("count"), 2);
            let second = list.GetCandidate(1).expect("second");
            assert_eq!(second.GetIndex().expect("index"), 1);
            assert_eq!(second.GetString().expect("text").to_string(), "加奈");

            let enumerator = list.EnumCandidates().expect("enumerator");
            let clone = enumerator.Clone().expect("clone");
            let mut slots = [None, None, None];
            let mut fetched = 0;
            enumerator
                .Next(&mut slots, &mut fetched)
                .expect("partial success remains a successful HRESULT");
            assert_eq!(fetched, 2);
            assert!(slots[2].is_none());

            let mut first = [None];
            clone.Next(&mut first, core::ptr::null_mut()).expect("one");
            assert_eq!(
                first[0]
                    .as_ref()
                    .expect("candidate")
                    .GetString()
                    .expect("text")
                    .to_string(),
                "仮名"
            );
        }
    }
}
