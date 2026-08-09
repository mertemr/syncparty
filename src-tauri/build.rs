fn main() {
    // The headless binary has no `tauri.conf.json`, icon set or frontend
    // bundle, so this would fail looking for assets it has no reason to carry.
    #[cfg(feature = "desktop")]
    tauri_build::build()
}
