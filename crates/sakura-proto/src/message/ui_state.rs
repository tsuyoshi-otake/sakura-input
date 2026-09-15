//! The renderer-facing UI snapshot carried by `Response::Ui`.

use crate::types::{
    AppearanceTheme, CandidateDetail, CandidateList, Mode, PadShortcut, ScreenRect,
};
use crate::Revision;

/// What the renderer draws, and the revision that identifies it.
///
/// Deliberately not a session's state. The renderer draws one indicator for
/// the whole logon session, because that is what the user sees — one caret,
/// in one focused field, at a time — while the engine keeps a mode per
/// session, one per focused field in every running application. This is the
/// mode of whichever session most recently changed one, which is the same
/// thing as "the mode of the field the user is typing in" for as long as
/// only one field can have the caret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiState {
    /// Increments on every change. A renderer passes the last one it saw
    /// back as `Request::WatchUi { since }`; the engine answers when this
    /// has moved past it.
    ///
    /// Starts at 1, so `since: 0` is always stale and always answers at
    /// once — that is a fresh renderer asking "what is true right now?".
    pub revision: Revision,
    /// User-wide appearance preference for Sakura-owned renderer UI. It is
    /// present even while the popup is hidden so a later candidate state
    /// cannot be drawn with an assumed palette.
    pub appearance_theme: AppearanceTheme,
    /// The user-wide Sakura Pad shortcut. It is carried even while the pad
    /// is hidden so the renderer can apply a changed preference before the
    /// next visible interaction.
    pub pad_shortcut: PadShortcut,
    /// The mode to show, or `None` when no field is composing and the
    /// indicator should be hidden.
    pub mode: Option<Mode>,
    /// Candidates for the renderer-owned popup, or `None` when conversion is
    /// not active. UI-less TSF hosts read the same list through
    /// `ITfCandidateListUIElement` in the DLL.
    pub candidates: Option<CandidateList>,
    /// Optional detail for the selected candidate. It is absent whenever the
    /// candidate list is absent, so renderers can fail closed without guessing.
    pub candidate_detail: Option<CandidateDetail>,
    /// Screen rectangle of the active composition. The renderer anchors its
    /// popup below this rectangle and hides it until one is available.
    pub anchor: Option<ScreenRect>,
    /// Screen rectangle of the host's editable area, when the host reports
    /// one. "Below the composition" is still inside the box the user is
    /// typing into whenever that box is taller than one caret line, so the
    /// renderer needs the box itself to avoid covering it.
    ///
    /// `None` whenever the host does not answer, which leaves the renderer
    /// with exactly the composition-only placement it used before.
    pub document: Option<ScreenRect>,
    /// `false` when TSF's UI-element manager elected to render candidates
    /// itself. The external renderer must then stay hidden.
    pub renderer_visible: bool,
    /// The engine is shutting down deliberately, and whoever is watching
    /// should shut down too rather than treat the closing pipe as a crash.
    ///
    /// This exists because the renderer is the engine's watchdog (DESIGN
    /// 3): when the pipe breaks it restarts the engine. That is right when
    /// the engine crashed and catastrophic during an uninstall, where
    /// `sakura_regtool --stop` has just asked it to exit and the installer
    /// is about to delete the very file the watchdog would relaunch — and
    /// a relaunched engine holds that file open, so the delete fails too.
    /// The two cases are indistinguishable from the broken pipe alone,
    /// which is why the intent is announced *before* the pipe breaks.
    ///
    /// It rides on the UI state rather than getting a message of its own
    /// because the renderer is already parked in a `WatchUi` call, and
    /// DESIGN 11 specifies `--stop` as asking the engine *and the renderer*
    /// to exit over the pipe — the renderer only holds the client end, so
    /// the request can only reach it as an answer to something it asked.
    pub stopping: bool,
}
