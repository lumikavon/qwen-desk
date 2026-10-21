; Remove only this product's login entry. User models/settings remain in LocalAppData.
!macro NSIS_HOOK_PREUNINSTALL
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Qwen Desk"
!macroend
