//! Private window-station clipboard round-trip tests for the Pad copy path
//! (K1, K2, K3).
//!
//! `SetProcessWindowStation` changes the whole process, so the real clipboard
//! work runs in a child process: this same test binary re-executed with
//! `SAKURA_PAD_CLIPBOARD_STATION_CHILD` set. The child creates its own window
//! station and desktop and a message-only window there, so nothing ever opens
//! the interactive desktop's clipboard.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GENERIC_ALL, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, CountClipboardFormats, GetClipboardData, IsClipboardFormatAvailable,
    OpenClipboard, RegisterClipboardFormatW,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, CloseWindowStation, CreateDesktopW, CreateWindowStationW,
    GetProcessWindowStation, GetThreadDesktop, SetProcessWindowStation, SetThreadDesktop,
    DESKTOP_CONTROL_FLAGS,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WS_CHILD,
};

use super::{copy_to_clipboard, place_text, ClipboardPrivacy};

const CHILD_ENV: &str = "SAKURA_PAD_CLIPBOARD_STATION_CHILD";
const CHILD_DEADLINE: Duration = Duration::from_secs(120);
const TEST_NAME: &str = "pad_clipboard::tests::private_station_clipboard_round_trip";
const TEXT: &str = "保護メモ 🌸\r\n2 行目";
/// The `WINSTA_ALL_ACCESS` mask (0x000F037F); the `windows` crate does not
/// carry the composed constant.
const WINSTA_ALL_ACCESS: u32 = 0x000F_037F;

/// K1/K2/K3: run the clipboard round trip inside a private window station.
///
/// The parent invocation re-runs this test alone in a child process with the
/// environment marker set and asserts exactly one test passed there; the
/// child invocation creates the private station, desktop and message-only
/// window and exercises T1 (K1), T2 (K2) and T3 (K3) in order.
#[test]
fn private_station_clipboard_round_trip() {
    if std::env::var_os(CHILD_ENV).is_none() {
        run_child_process();
        return;
    }
    std::thread::spawn(station_thread_main)
        .join()
        .expect("private-station thread panicked");
}

fn run_child_process() {
    let exe = std::env::current_exe().expect("current test exe");
    let mut child = Command::new(exe)
        .args([TEST_NAME, "--exact", "--test-threads=1", "--nocapture"])
        .env(CHILD_ENV, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn child test process");
    let deadline = Instant::now() + CHILD_DEADLINE;
    let finished = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => break false,
            Err(error) => panic!("waiting on the child failed: {error}"),
        }
    };
    if !finished {
        let _ = child.kill();
        let output = child.wait_with_output().expect("killed child output");
        panic!(
            "child test process did not finish within {:?}\nstdout:\n{}\nstderr:\n{}",
            CHILD_DEADLINE,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    let output = child.wait_with_output().expect("child output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "child test process failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout,
        stderr,
    );
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "child filter did not run exactly one passing test\nstdout:\n{}\nstderr:\n{}",
        stdout,
        stderr,
    );
}

fn station_thread_main() {
    // SAFETY: the private station and desktop are created, used, and torn
    // down on this one thread; the guards restore the process station and
    // thread desktop even on panic, and the window dies with its desktop.
    unsafe {
        let original_station = GetProcessWindowStation().expect("process window station");
        let private_station = CreateWindowStationW(PCWSTR::null(), 0, WINSTA_ALL_ACCESS, None)
            .expect("create private window station");
        let _station = StationGuard {
            original: original_station,
            private: private_station,
        };
        SetProcessWindowStation(private_station).expect("set process window station");

        let original_desktop = GetThreadDesktop(GetCurrentThreadId()).expect("thread desktop");
        let private_desktop = CreateDesktopW(
            w!("sakura-pad-clipboard-test"),
            None,
            None,
            DESKTOP_CONTROL_FLAGS(0),
            GENERIC_ALL.0,
            None,
        )
        .expect("create private desktop");
        SetThreadDesktop(private_desktop).expect("set thread desktop");
        let _desktop = DesktopGuard {
            original: original_desktop,
            private: private_desktop,
        };

        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            PCWSTR::null(),
            WS_CHILD,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
        .expect("create message-only window");
        let _window = WindowGuard(window);

        t1_protected_copy_is_marked_and_pasteable(window);
        t2_normal_copy_is_text_only(window);
        t3_marker_failure_empties_the_clipboard(window);
    }
}

struct StationGuard {
    original: windows::Win32::System::StationsAndDesktops::HWINSTA,
    private: windows::Win32::System::StationsAndDesktops::HWINSTA,
}

impl Drop for StationGuard {
    fn drop(&mut self) {
        // SAFETY: both handles are live window stations owned by this test.
        unsafe {
            let _ = SetProcessWindowStation(self.original);
            let _ = CloseWindowStation(self.private);
        }
    }
}

struct DesktopGuard {
    original: windows::Win32::System::StationsAndDesktops::HDESK,
    private: windows::Win32::System::StationsAndDesktops::HDESK,
}

impl Drop for DesktopGuard {
    fn drop(&mut self) {
        // SAFETY: the thread is still on the private desktop; restore first,
        // then close the private handle.
        unsafe {
            let _ = SetThreadDesktop(self.original);
            let _ = CloseDesktop(self.private);
        }
    }
}

struct WindowGuard(HWND);

impl Drop for WindowGuard {
    fn drop(&mut self) {
        // SAFETY: the window is this thread's own message-only window.
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

fn register(name: &str) -> u32 {
    let mut wide: Vec<u16> = name.encode_utf16().collect();
    wide.push(0);
    // SAFETY: `wide` is a live nul-terminated UTF-16 buffer.
    unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) }
}

/// T1 (K1): a protected copy returns true, the text is pasteable as
/// CF_UNICODETEXT, and the same session carries
/// ExcludeClipboardContentFromMonitorProcessing (=1),
/// CanIncludeInClipboardHistory (=0), and CanUploadToCloudClipboard (=0).
fn t1_protected_copy_is_marked_and_pasteable(hwnd: HWND) {
    assert!(copy_to_clipboard(hwnd, TEXT, ClipboardPrivacy::Protected));
    assert_eq!(clipboard_text(hwnd).as_deref(), Some(TEXT));
    let exclude = register("ExcludeClipboardContentFromMonitorProcessing");
    assert_ne!(exclude, 0);
    // SAFETY: the clipboard is queried from this thread only.
    assert!(unsafe { IsClipboardFormatAvailable(exclude) }.is_ok());
    assert_eq!(marker_dword(hwnd, "CanIncludeInClipboardHistory"), Some(0));
    assert_eq!(marker_dword(hwnd, "CanUploadToCloudClipboard"), Some(0));
}

/// T2 (K2): a normal copy places only CF_UNICODETEXT, exactly as before the
/// marker change.
fn t2_normal_copy_is_text_only(hwnd: HWND) {
    assert!(copy_to_clipboard(hwnd, TEXT, ClipboardPrivacy::Normal));
    assert_eq!(clipboard_text(hwnd).as_deref(), Some(TEXT));
    for name in [
        "ExcludeClipboardContentFromMonitorProcessing",
        "CanIncludeInClipboardHistory",
        "CanUploadToCloudClipboard",
    ] {
        let format = register(name);
        assert_ne!(format, 0);
        // SAFETY: the clipboard is queried from this thread only.
        assert!(unsafe { IsClipboardFormatAvailable(format) }.is_err());
    }
}

/// T3 (K3): a marker that cannot be registered fails the whole copy closed —
/// false, an empty clipboard, and a closable clipboard afterwards.
fn t3_marker_failure_empties_the_clipboard(hwnd: HWND) {
    for index in 0..3 {
        let mut markers = ClipboardPrivacy::Protected.markers().to_vec();
        markers[index].name = "";
        assert!(
            !place_text(hwnd, "x", &markers),
            "marker {index} with an unregistrable name must fail the copy"
        );
        // SAFETY: the clipboard is queried from this thread only.
        unsafe {
            OpenClipboard(Some(hwnd)).expect("clipboard opens after a failed copy");
            assert_eq!(CountClipboardFormats(), 0);
            let _ = CloseClipboard();
        }
    }
}

fn clipboard_text(hwnd: HWND) -> Option<String> {
    // SAFETY: every handle is checked, the block stays locked only inside the
    // read, and the clipboard is closed on every path.
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let block = GetClipboardData(windows::Win32::System::Ole::CF_UNICODETEXT.0 as u32).ok();
        let text = block.and_then(|handle| {
            let data = GlobalLock(HGLOBAL(handle.0));
            if data.is_null() {
                None
            } else {
                let mut units = 0;
                while *data.cast::<u16>().add(units) != 0 {
                    units += 1;
                }
                Some(String::from_utf16_lossy(std::slice::from_raw_parts(
                    data.cast::<u16>(),
                    units,
                )))
            }
        });
        if let Some(handle) = block {
            let _ = GlobalUnlock(HGLOBAL(handle.0));
        }
        let _ = CloseClipboard();
        text
    }
}

fn marker_dword(hwnd: HWND, name: &str) -> Option<u32> {
    let format = register(name);
    assert_ne!(format, 0);
    // SAFETY: the block is read as the 4-byte payload the writer allocated.
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        if IsClipboardFormatAvailable(format).is_err() {
            let _ = CloseClipboard();
            return None;
        }
        let block = GetClipboardData(format).ok();
        let value = block.and_then(|handle| {
            let data = GlobalLock(HGLOBAL(handle.0));
            if data.is_null() {
                None
            } else {
                Some(*data.cast::<u32>())
            }
        });
        if let Some(handle) = block {
            let _ = GlobalUnlock(HGLOBAL(handle.0));
        }
        let _ = CloseClipboard();
        value
    }
}
