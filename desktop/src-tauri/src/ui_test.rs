// Cocoa requires the process main thread, which the standard Rust test harness
// does not provide. Each scenario launches in its own temporary home.
#[cfg(target_os = "macos")]
#[allow(dead_code, unused_imports)]
#[path = "main.rs"]
mod desktop;

fn main() {
    #[cfg(target_os = "macos")]
    desktop::tests::native_ui::run().unwrap();
}
