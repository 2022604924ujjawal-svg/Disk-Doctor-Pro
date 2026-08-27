; Tauri's default NSIS template enables custom header images but leaves them
; aligned to the left. MangoDisk's compact brand mark is intentionally placed
; in the bitmap's right-side safe area, so force right alignment on installer
; and uninstaller inner pages. Keeping this as a small hook avoids maintaining
; a fork of Tauri's complete installer template.
!define MUI_HEADERIMAGE_RIGHT

; Tauri stores the selected NSIS installation directory below a registry key
; derived from `bundle.publisher`. MangoDisk 1.0.0 and 1.0.1 used `harry0703`,
; while 1.0.2 and 1.0.3 used the GitHub profile URL. An automatic update built
; with the stable `MangoDisk` publisher cannot discover either custom install
; directory through Tauri's new publisher key, so it would fall back to
; `%LOCALAPPDATA%\MangoDisk` and leave the existing shortcut on the old binary.
;
; Limit the compatibility lookup to updater-driven installs. Interactive
; installers must continue to respect the directory explicitly chosen by the
; user. The URL publisher is checked first because it belongs to the newer
; releases; the original publisher is only a fallback for direct upgrades from
; 1.0.0 or 1.0.1. Requiring the expected executable prevents a stale registry
; value from redirecting installation into a missing or unrelated directory.
!macro NSIS_HOOK_PREINSTALL
  ${If} $UpdateMode = 1
    ReadRegStr $R8 SHCTX "Software\https://github.com/harry0703\MangoDisk" ""

    ${If} $R8 == ""
      ReadRegStr $R8 SHCTX "Software\harry0703\MangoDisk" ""
    ${EndIf}

    ${If} $R8 != ""
    ${AndIf} ${FileExists} "$R8\${MAINBINARYNAME}.exe"
      StrCpy $INSTDIR $R8

      ; Tauri selects the output directory before expanding this hook. Reset it
      ; after changing `$INSTDIR` so every bundled file is copied to the restored
      ; location rather than the publisher's new default directory.
      SetOutPath $INSTDIR
      DetailPrint "Restored the existing MangoDisk installation directory for publisher migration"
    ${EndIf}
  ${EndIf}
!macroend
