#![cfg(windows)]
// A console window flashing up at logon, in front of whatever the user is
// doing, for a process that has no console output, is not acceptable. Only
// outside tests, which need the ordinary test-harness console.
#![cfg_attr(not(test), windows_subsystem = "windows")]

fn main() -> windows::core::Result<()> {
    sakura_renderer::run()
}
