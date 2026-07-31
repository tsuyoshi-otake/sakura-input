//! How the preedit is drawn.
//!
//! TSF does not let a text service paint. Instead the service publishes a small
//! set of named styles, the application looks them up through
//! `ITfDisplayAttributeProvider`, and the text is drawn by the application in its
//! own font and colours. That indirection is why a preedit looks native in
//! Notepad and in Word without either of them knowing anything about this IME.
//!
//! Colours are deliberately left as "no colour" so the host picks its own; only
//! the underline shape and the semantic `TF_DA_ATTR_INFO` are specified, which is
//! the part that actually distinguishes unconverted text from a conversion
//! candidate the user is looking at.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{E_INVALIDARG, E_POINTER, E_UNEXPECTED, S_FALSE};
use windows::Win32::UI::TextServices::{
    IEnumTfDisplayAttributeInfo, IEnumTfDisplayAttributeInfo_Impl, ITfDisplayAttributeInfo,
    ITfDisplayAttributeInfo_Impl, TF_ATTR_CONVERTED, TF_ATTR_INPUT, TF_ATTR_TARGET_CONVERTED,
    TF_CT_NONE, TF_DA_ATTR_INFO, TF_DA_COLOR, TF_DA_COLOR_0, TF_DA_LINESTYLE, TF_DISPLAYATTRIBUTE,
    TF_LS_DOT, TF_LS_SOLID,
};
use windows_core::{implement, Error, IUnknownImpl, Result, BOOL, BSTR, GUID};

use sakura_reg::{
    GUID_DISPLAY_ATTRIBUTE_CONVERTED, GUID_DISPLAY_ATTRIBUTE_FOCUSED, GUID_DISPLAY_ATTRIBUTE_RAW,
};

/// One published style.
#[derive(Clone, Copy)]
struct Descriptor {
    guid: GUID,
    description: &'static str,
    attribute: TF_DISPLAYATTRIBUTE,
}

impl core::fmt::Debug for Descriptor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Descriptor")
            .field("guid", &self.guid)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

/// "Let the host choose." Every colour we publish is this: an IME that hardcodes
/// black text is unreadable the moment the user switches to a dark theme.
const fn host_colour() -> TF_DA_COLOR {
    TF_DA_COLOR {
        r#type: TF_CT_NONE,
        Anonymous: TF_DA_COLOR_0 { nIndex: 0 },
    }
}

const fn style(line: TF_DA_LINESTYLE, bold: bool, kind: TF_DA_ATTR_INFO) -> TF_DISPLAYATTRIBUTE {
    TF_DISPLAYATTRIBUTE {
        crText: host_colour(),
        crBk: host_colour(),
        lsStyle: line,
        fBoldLine: BOOL(bold as i32),
        crLine: host_colour(),
        bAttr: kind,
    }
}

/// The three states a run of composition text can be in, in the order TSF
/// enumerates them. Matching the Microsoft IME's visual vocabulary here is not
/// cosmetic: users read the underline to know what pressing Space will do.
const DESCRIPTORS: [Descriptor; 3] = [
    Descriptor {
        guid: GUID_DISPLAY_ATTRIBUTE_RAW,
        description: "Sakura Input: unconverted",
        attribute: style(TF_LS_DOT, false, TF_ATTR_INPUT),
    },
    Descriptor {
        guid: GUID_DISPLAY_ATTRIBUTE_CONVERTED,
        description: "Sakura Input: converted",
        attribute: style(TF_LS_SOLID, false, TF_ATTR_CONVERTED),
    },
    Descriptor {
        guid: GUID_DISPLAY_ATTRIBUTE_FOCUSED,
        description: "Sakura Input: focused clause",
        attribute: style(TF_LS_SOLID, true, TF_ATTR_TARGET_CONVERTED),
    },
];

fn descriptor_for(guid: &GUID) -> Option<Descriptor> {
    DESCRIPTORS
        .iter()
        .find(|entry| entry.guid == *guid)
        .copied()
}

/// A single published style, as TSF sees it.
///
/// Applications are allowed to override a style and to reset it, so the current
/// value is mutable while the descriptor it came from is not.
#[implement(ITfDisplayAttributeInfo)]
struct DisplayAttributeInfo {
    descriptor: Descriptor,
    current: RefCell<TF_DISPLAYATTRIBUTE>,
}

impl DisplayAttributeInfo {
    fn new(descriptor: Descriptor) -> Self {
        Self {
            descriptor,
            current: RefCell::new(descriptor.attribute),
        }
    }
}

impl core::fmt::Debug for DisplayAttributeInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DisplayAttributeInfo")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

impl ITfDisplayAttributeInfo_Impl for DisplayAttributeInfo_Impl {
    fn GetGUID(&self) -> Result<GUID> {
        Ok(self.get_impl().descriptor.guid)
    }

    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from(self.get_impl().descriptor.description))
    }

    fn GetAttributeInfo(&self, pda: *mut TF_DISPLAYATTRIBUTE) -> Result<()> {
        if pda.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        let current = *self
            .get_impl()
            .current
            .try_borrow()
            .map_err(|_| Error::new(E_UNEXPECTED, "re-entrant display attribute access"))?;
        // SAFETY: `pda` was just checked non-null and TSF guarantees it points at
        // a writable `TF_DISPLAYATTRIBUTE`.
        unsafe { pda.write(current) };
        Ok(())
    }

    fn SetAttributeInfo(&self, pda: *const TF_DISPLAYATTRIBUTE) -> Result<()> {
        if pda.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        // SAFETY: `pda` was just checked non-null and TSF guarantees it points at
        // a readable `TF_DISPLAYATTRIBUTE` for the duration of the call.
        let value = unsafe { pda.read() };
        let mut current = self
            .get_impl()
            .current
            .try_borrow_mut()
            .map_err(|_| Error::new(E_UNEXPECTED, "re-entrant display attribute access"))?;
        *current = value;
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        let implementation = self.get_impl();
        let mut current = implementation
            .current
            .try_borrow_mut()
            .map_err(|_| Error::new(E_UNEXPECTED, "re-entrant display attribute access"))?;
        *current = implementation.descriptor.attribute;
        Ok(())
    }
}

/// Walks [`DESCRIPTORS`] for a host that asked for the whole set.
#[implement(IEnumTfDisplayAttributeInfo)]
#[derive(Debug)]
struct DisplayAttributeEnumerator {
    position: Cell<usize>,
}

impl DisplayAttributeEnumerator {
    fn at(position: usize) -> Self {
        Self {
            position: Cell::new(position),
        }
    }
}

impl IEnumTfDisplayAttributeInfo_Impl for DisplayAttributeEnumerator_Impl {
    fn Clone(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(DisplayAttributeEnumerator::at(self.get_impl().position.get()).into())
    }

    fn Next(
        &self,
        ulcount: u32,
        rginfo: *mut Option<ITfDisplayAttributeInfo>,
        pcfetched: *mut u32,
    ) -> Result<()> {
        if rginfo.is_null() && ulcount != 0 {
            return Err(Error::from_hresult(E_POINTER));
        }

        let start = self.get_impl().position.get();
        let mut fetched = 0usize;
        while (fetched as u32) < ulcount {
            let Some(descriptor) = DESCRIPTORS.get(start + fetched) else {
                break;
            };
            let info: ITfDisplayAttributeInfo = DisplayAttributeInfo::new(*descriptor).into();
            // SAFETY: the caller guarantees `rginfo` addresses `ulcount` writable
            // slots, and `fetched` is below that bound. `write` rather than an
            // assignment because the slots may be uninitialized, and dropping
            // whatever bit pattern happened to be there would be a wild release.
            unsafe { rginfo.add(fetched).write(Some(info)) };
            fetched += 1;
        }
        self.get_impl().position.set(start + fetched);

        if !pcfetched.is_null() {
            // SAFETY: checked non-null; TSF guarantees a writable `u32`.
            unsafe { pcfetched.write(fetched as u32) };
        }

        if fetched as u32 == ulcount {
            Ok(())
        } else {
            // `S_FALSE` is a success code, so it has to travel as an "error" to
            // survive the `Result` -> `HRESULT` conversion the vtable performs.
            // Returning `Ok(())` here would tell the host it got a full buffer.
            Err(Error::from_hresult(S_FALSE))
        }
    }

    fn Reset(&self) -> Result<()> {
        self.get_impl().position.set(0);
        Ok(())
    }

    fn Skip(&self, ulcount: u32) -> Result<()> {
        let position = self.get_impl().position.get();
        let skipped = position.saturating_add(ulcount as usize);
        self.get_impl().position.set(skipped.min(DESCRIPTORS.len()));
        if skipped <= DESCRIPTORS.len() {
            Ok(())
        } else {
            Err(Error::from_hresult(S_FALSE))
        }
    }
}

/// Every style this text service publishes, for `EnumDisplayAttributeInfo`.
pub fn enumerate() -> IEnumTfDisplayAttributeInfo {
    DisplayAttributeEnumerator::at(0).into()
}

/// One style by GUID, for `GetDisplayAttributeInfo`.
pub fn lookup(guid: &GUID) -> Result<ITfDisplayAttributeInfo> {
    match descriptor_for(guid) {
        Some(descriptor) => Ok(DisplayAttributeInfo::new(descriptor).into()),
        None => Err(Error::from_hresult(E_INVALIDARG)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_published_guid_resolves() {
        for descriptor in DESCRIPTORS {
            let found = descriptor_for(&descriptor.guid);
            assert!(
                found.is_some(),
                "{} is enumerated but not resolvable",
                descriptor.description
            );
        }
    }

    #[test]
    fn unknown_guid_does_not_resolve() {
        assert!(descriptor_for(&GUID::zeroed()).is_none());
    }

    #[test]
    fn guids_are_distinct() {
        for (index, descriptor) in DESCRIPTORS.iter().enumerate() {
            for other in DESCRIPTORS.iter().skip(index + 1) {
                assert_ne!(
                    descriptor.guid, other.guid,
                    "{} and {} share a GUID",
                    descriptor.description, other.description
                );
            }
        }
    }

    /// The underline is the only thing telling the user which state a clause is
    /// in, so two states rendering identically is a usability bug, not a detail.
    #[test]
    fn states_are_visually_distinguishable() {
        for (index, descriptor) in DESCRIPTORS.iter().enumerate() {
            for other in DESCRIPTORS.iter().skip(index + 1) {
                let same_line = descriptor.attribute.lsStyle == other.attribute.lsStyle
                    && descriptor.attribute.fBoldLine.as_bool()
                        == other.attribute.fBoldLine.as_bool();
                assert!(
                    !same_line,
                    "{} and {} draw the same underline",
                    descriptor.description, other.description
                );
            }
        }
    }
}
