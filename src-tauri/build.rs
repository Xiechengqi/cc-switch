fn main() {
    inject_build_metadata();

    tauri_build::build();

    // Windows: Embed Common Controls v6 manifest for test binaries
    //
    // When running `cargo test`, the generated test executables don't include
    // the standard Tauri application manifest. Without Common Controls v6,
    // `tauri::test` calls fail with STATUS_ENTRYPOINT_NOT_FOUND.
    //
    // This workaround:
    // 1. Embeds the manifest into test binaries via /MANIFEST:EMBED
    // 2. Uses /MANIFEST:NO for the main binary to avoid duplicate resources
    //    (Tauri already handles manifest embedding for the app binary)
    #[cfg(target_os = "windows")]
    {
        let manifest_path = std::path::PathBuf::from(
            std::env::var("CARGO_MANIFEST_DIR").expect("missing CARGO_MANIFEST_DIR"),
        )
        .join("common-controls.manifest");
        let manifest_arg = format!("/MANIFESTINPUT:{}", manifest_path.display());

        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg={}", manifest_arg);
        // Avoid duplicate manifest resources in binary builds.
        println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
        println!("cargo:rerun-if-changed={}", manifest_path.display());
    }
}

fn inject_build_metadata() {
    // Commit SHA: prefer env override (CI sets the main-branch HEAD before any
    // version-bump orphan commit), fall back to git in the working tree, then to
    // an explicit "unknown" sentinel that the frontend uses to hide the badge.
    let sha = std::env::var("CC_SWITCH_BUILD_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("GITHUB_SHA").ok().filter(|s| !s.is_empty()))
        .or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                    } else {
                        None
                    }
                })
        })
        .unwrap_or_else(|| "unknown".to_string());

    // Build time in RFC3339 UTC. Allow override (deterministic builds, CI).
    let build_time = std::env::var("CC_SWITCH_BUILD_TIME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        });

    println!("cargo:rustc-env=CC_SWITCH_BUILD_SHA={sha}");
    println!("cargo:rustc-env=CC_SWITCH_BUILD_TIME={build_time}");
    println!("cargo:rerun-if-env-changed=CC_SWITCH_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=CC_SWITCH_BUILD_TIME");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    // Re-run when git HEAD or refs change so dev rebuilds pick up new commits.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs");
}
