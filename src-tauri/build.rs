fn main() {
    // Tauri embeds native icons into the compiled application, but icon-only changes do not
    // automatically invalidate the generated context in every development workflow. Watching the
    // directory ensures `tauri dev` rebuilds the executable after a Dock or taskbar icon update.
    println!("cargo:rerun-if-changed=icons");

    // Keep the Windows GUI in the interactive user's security context. If a standard user
    // approves UAC with a different administrator account, WebView2 runs as the interactive
    // user while the elevated host resolves its data directory from the administrator's
    // LocalAppData. That ACL mismatch prevents startup. The manifest therefore uses `asInvoker`;
    // operations requiring administrator rights must elevate only at their platform capability
    // boundary.
    let windows =
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("windows/app.manifest"));
    let attributes = tauri_build::Attributes::new().windows_attributes(windows);

    tauri_build::try_build(attributes).expect("failed to run the Tauri build script");
}
