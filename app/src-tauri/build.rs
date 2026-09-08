// Compiles the GitHub OAuth App's device-flow client id in from
// app/.env.local (gitignored) so dev and release builds alike get it without
// an exported env var. `option_env!` in lib.rs then sees it as
// `VIGIE_GITHUB_CLIENT_ID`; a build with no .env.local compiles the
// "REPLACE_ME" placeholder and the app disables its Sign in button.
fn main() {
    let env_file = concat!(env!("CARGO_MANIFEST_DIR"), "/../.env.local");
    if let Ok(contents) = std::fs::read_to_string(env_file) {
        for line in contents.lines() {
            if let Some(value) = line.strip_prefix("VIGIE_GITHUB_CLIENT_ID=") {
                let value = value.trim();
                if !value.is_empty() {
                    println!("cargo:rustc-env=VIGIE_GITHUB_CLIENT_ID={value}");
                    println!("cargo:rerun-if-changed={env_file}");
                }
                break;
            }
        }
    }
    tauri_build::build()
}
