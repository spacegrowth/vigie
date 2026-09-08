// Prevents an additional console window on Windows in release builds; a
// no-op on macOS but harmless to keep for parity with the standard scaffold.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    gitmon_app_lib::run();
}
