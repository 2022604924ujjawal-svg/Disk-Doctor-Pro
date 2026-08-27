# Windows MSI uninstall fixture

This source fixture creates one MangoDisk-owned, per-user MSI installation. It
exists only to verify the Windows native-uninstall boundary. Never run native
uninstall tests against an unrelated product code.

The generated `fixture.msi` is a local test artifact and must not be committed.
Build it with WiX Toolset 4:

```powershell
wix build fixture.wxs -arch arm64 -o fixture.msi
```

Use `-arch x64` on an x64 Windows test machine. Install the package explicitly,
then run the ignored fixture tests:

```powershell
msiexec.exe /i fixture.msi /qn /norestart
cargo test --manifest-path src-tauri/Cargo.toml -p mangodisk-platform `
  real_fixture_reports_current_user_scope -- --ignored --exact
cargo test --manifest-path src-tauri/Cargo.toml -p mangodisk-platform `
  real_fixture_reports_installed_state -- --ignored --exact
```

The destructive test removes only this fixture:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml -p mangodisk-platform `
  real_fixture_uninstalls_and_verifies_absence -- --ignored --exact
```

Product code:
`{9627E855-337D-45EC-A2D9-CBB92B447399}`.
