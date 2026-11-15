; astral-dev — fin d'installation / de désinstallation non ambiguë.

!macro NSIS_HOOK_POSTINSTALL
  ; Passe directement à la page « Terminer » (au lieu d'attendre un clic sur Suivant).
  SetAutoClose true
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Message final explicite, sauf en mode silencieux ou lors d'une mise à jour.
  ${If} $UpdateMode <> 1
  ${AndIfNot} ${Silent}
    ${If} $LANGUAGE = 1036
      MessageBox MB_OK|MB_ICONINFORMATION "astral-dev a été désinstallé avec succès.$\r$\n$\r$\nVos projets et données dans C:\astral-dev ont été conservés (à supprimer à la main si besoin)."
    ${Else}
      MessageBox MB_OK|MB_ICONINFORMATION "astral-dev was uninstalled successfully.$\r$\n$\r$\nYour projects and data in C:\astral-dev were kept (delete them manually if needed)."
    ${EndIf}
  ${EndIf}
  SetAutoClose true
!macroend
