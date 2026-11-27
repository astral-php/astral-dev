; Rustral Studio — fin d'installation / de désinstallation non ambiguë.

; Ferme Rustral Studio (et l'ancien astral-dev) avant d'installer ou de désinstaller (un exécutable en cours d'exécution est verrouillé :
; c'est la cause probable de « impossible de désinstaller la version précédente »). Les services éventuels
; restent actifs et seront repris comme « session précédente » au prochain lancement.
!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec 'taskkill /IM rustral-studio.exe /F'
  Pop $0
  ${If} $UpdateMode <> 1
    nsExec::Exec 'taskkill /IM astral-dev.exe /F'
    Pop $0
  ${EndIf}
  Sleep 800
  ; Ancienne installation « astral-dev » (autre identifiant Windows) : proposer de la désinstaller. Les données
  ; (C:\astral-dev) ne sont jamais touchées. Deux applications ne doivent pas piloter les mêmes données (ADR-019).
  ReadRegStr $R0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\astral-dev" "UninstallString"
  ${If} $UpdateMode <> 1
  ${AndIf} $R0 != ""
  ${AndIfNot} ${Silent}
    MessageBox MB_YESNO|MB_ICONQUESTION "Une ancienne installation d'astral-dev a été trouvée.$\r$\n$\r$\nRustral Studio la remplace : la désinstaller maintenant ? Vos projets et données (C:\astral-dev) sont conservés et seront réutilisés." IDNO rs_skip_old
    ExecWait '$R0 /S _?=$LOCALAPPDATA\astral-dev'
    Delete "$LOCALAPPDATA\astral-dev\uninstall.exe"
    RMDir "$LOCALAPPDATA\astral-dev"
    rs_skip_old:
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'taskkill /IM rustral-studio.exe /F'
  Pop $0
  nsExec::Exec 'taskkill /IM astral-dev.exe /F'
  Pop $0
  Sleep 800
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Passe directement à la page « Terminer » (au lieu d'attendre un clic sur Suivant).
  SetAutoClose true
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Message final explicite, sauf en mode silencieux ou lors d'une mise à jour.
  ${If} $UpdateMode <> 1
  ${AndIfNot} ${Silent}
    ${If} $LANGUAGE = 1036
      MessageBox MB_OK|MB_ICONINFORMATION "Rustral Studio a été désinstallé avec succès.$\r$\n$\r$\nVos projets et données (C:\rustral-studio ou C:\astral-dev) ont été conservés (à supprimer à la main si besoin)."
    ${Else}
      MessageBox MB_OK|MB_ICONINFORMATION "Rustral Studio was uninstalled successfully.$\r$\n$\r$\nYour projects and data (C:\rustral-studio or C:\astral-dev) were kept (delete them manually if needed)."
    ${EndIf}
  ${EndIf}
  SetAutoClose true
!macroend
