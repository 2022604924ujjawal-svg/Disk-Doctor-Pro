on run argv
  set targetPath to item 1 of argv
  set stagingRoot to item 2 of argv
  set stagedTarget to item 3 of argv
  set expectedIdentity to item 4 of argv
  set promptText to item 5 of argv
  set itemChangedStatus to item 6 of argv
  set recoveryRequiredStatus to item 7 of argv
  set successResponse to item 8 of argv
  set errorResponsePrefix to item 9 of argv

  -- Every filesystem value crosses the shell boundary as one quoted argument.
  -- Keeping these conversions beside the parameter reads makes later command
  -- edits less likely to introduce an unquoted path.
  set quotedTargetPath to quoted form of targetPath
  set quotedStagingRoot to quoted form of stagingRoot
  set quotedStagedTarget to quoted form of stagedTarget
  set quotedExpectedIdentity to quoted form of expectedIdentity
  set quotedItemChangedStatus to quoted form of itemChangedStatus
  set quotedRecoveryRequiredStatus to quoted form of recoveryRequiredStatus

  try
    set removeCommand to "umask 077; "
    set removeCommand to removeCommand & "/bin/mkdir " & quotedStagingRoot & " || exit 40; "
    set removeCommand to removeCommand & "/bin/mv " & quotedTargetPath & " " & quotedStagedTarget
    set removeCommand to removeCommand & " || { /bin/rmdir " & quotedStagingRoot & " >/dev/null 2>&1; exit 41; }; "
    set removeCommand to removeCommand & "actualIdentity=$(/usr/bin/stat -f '%d:%i' " & quotedStagedTarget & " 2>/dev/null); "

    -- A changed object is restored before returning a typed stale-item result.
    -- Any ambiguous restore state is retained for explicit recovery inspection.
    set removeCommand to removeCommand & "if test \"$actualIdentity\" != " & quotedExpectedIdentity & "; then "
    set removeCommand to removeCommand & "if test -e " & quotedTargetPath & " || test -L " & quotedTargetPath & "; then exit " & quotedRecoveryRequiredStatus & "; fi; "
    set removeCommand to removeCommand & "/bin/mv " & quotedStagedTarget & " " & quotedTargetPath & " || exit " & quotedRecoveryRequiredStatus & "; "
    set removeCommand to removeCommand & "if test -e " & quotedStagedTarget & " || test -L " & quotedStagedTarget & "; then exit " & quotedRecoveryRequiredStatus & "; fi; "
    set removeCommand to removeCommand & "/bin/rmdir " & quotedStagingRoot & " || exit " & quotedRecoveryRequiredStatus & "; "
    set removeCommand to removeCommand & "exit " & quotedItemChangedStatus & "; fi; "

    set removeCommand to removeCommand & "/bin/rm -rf " & quotedStagedTarget & "; removeStatus=$?; "
    set removeCommand to removeCommand & "if test $removeStatus -ne 0 || test -e " & quotedStagedTarget & " || test -L " & quotedStagedTarget & "; then "
    set removeCommand to removeCommand & "if test -e " & quotedTargetPath & " || test -L " & quotedTargetPath & "; then exit " & quotedRecoveryRequiredStatus & "; fi; "
    set removeCommand to removeCommand & "/bin/mv " & quotedStagedTarget & " " & quotedTargetPath & " || exit " & quotedRecoveryRequiredStatus & "; "
    set removeCommand to removeCommand & "/bin/rmdir " & quotedStagingRoot & " || exit " & quotedRecoveryRequiredStatus & "; "
    set removeCommand to removeCommand & "exit " & quotedRecoveryRequiredStatus & "; fi; "
    set removeCommand to removeCommand & "/bin/rmdir " & quotedStagingRoot & " || exit 44"

    if promptText is "" then
      do shell script removeCommand with administrator privileges
    else
      do shell script removeCommand with prompt promptText with administrator privileges
    end if
    return successResponse
  on error errorMessage number errorNumber
    return errorResponsePrefix & errorNumber
  end try
end run
