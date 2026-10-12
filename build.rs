use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=XRUN_CLOUDFLARE_BUNDLE_DIR");
    let oauth_path = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("cloudflare/oauth.json");
    println!("cargo:rerun-if-changed={}", oauth_path.display());
    let oauth: serde_json::Value =
        serde_json::from_slice(&fs::read(oauth_path).expect("Cloudflare OAuth configuration"))
            .expect("Cloudflare OAuth configuration JSON");
    let client_id = oauth["client_id"]
        .as_str()
        .filter(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .expect("Cloudflare OAuth client ID must be 32 hexadecimal characters");
    println!("cargo:rustc-env=XRUN_CLOUDFLARE_PUBLIC_CLIENT_ID={client_id}");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let generated = output.join("cloudflare_bundle.rs");
    let Some(directory) = env::var_os("XRUN_CLOUDFLARE_BUNDLE_DIR") else {
        fs::write(generated, "const EMBEDDED: Option<(&str, &str)> = None;\n").unwrap();
        return;
    };
    let directory = PathBuf::from(directory)
        .canonicalize()
        .expect("Worker bundle directory");
    let manifest_path = directory.join("manifest.json");
    let source_path = directory.join("relay.mjs");
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    println!("cargo:rerun-if-changed={}", source_path.display());
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("Worker manifest"))
            .expect("Worker manifest JSON");
    let source = fs::read(&source_path).expect("Worker module");
    assert_eq!(manifest["schema"], 1, "Worker manifest schema");
    assert_eq!(
        manifest["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION")),
        "Worker release version"
    );
    assert_eq!(
        manifest["sha256"].as_str(),
        Some(
            Sha256::digest(&source)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
                .as_str()
        ),
        "Worker checksum"
    );
    assert!(
        !source.is_empty() && source.len() <= 10 * 1024 * 1024,
        "Worker module size"
    );
    fs::copy(source_path, output.join("relay.mjs")).unwrap();
    fs::copy(manifest_path, output.join("manifest.json")).unwrap();
    fs::write(
        generated,
        concat!(
            "const EMBEDDED: Option<(&str, &str)> = Some((",
            "include_str!(concat!(env!(\"OUT_DIR\"), \"/manifest.json\")),",
            "include_str!(concat!(env!(\"OUT_DIR\"), \"/relay.mjs\"))));\n",
        ),
    )
    .unwrap();
}
