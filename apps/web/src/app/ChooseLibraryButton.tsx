import { Library, Plus } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router-dom'

import { useApp } from '@/shared/contexts/app-context'
import { Button } from '@/shared/ui'
import { usePreferences } from './preferences-context'
import { useScopeSwitcher } from './scope-switcher-context'

/// The one control that resolves "no library selected", wherever a screen has
/// to report it.
///
/// Which control that is depends on why nothing is selected. With libraries to
/// choose from it is the shell's own picker. With none at all the picker is the
/// wrong answer: it opens on an empty list that reads "no matches", as if a
/// filter were hiding something, so an administrator is sent to create one
/// instead. A reader who can neither pick nor create gets no button, because
/// there is nothing here for them to press — the surrounding copy tells them
/// who can help.
///
/// The role comes from `usePreferences`, which is what `AdminLayout` admits on.
/// Reading the session role instead would let this button offer a destination
/// that then refuses the reader — a dead end grown out of the fix for one.
///
/// `libraries` is the bounded shell sample, not the full set: an empty sample
/// is taken to mean the reader has none, which is the only reading that leaves
/// them somewhere to go.
export function ChooseLibraryButton() {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const { libraries } = useApp()
  const { role } = usePreferences()
  const scope = useScopeSwitcher()

  if (libraries.length > 0) {
    if (!scope) return null
    return (
      <Button variant="primary" onClick={scope.openScopeSwitcher}>
        <Library size={15} />
        {t('shell.chooseLibrary')}
      </Button>
    )
  }

  if (role !== 'admin') return null
  return (
    <Button
      variant="primary"
      onClick={() => {
        void navigate('/admin/libraries')
      }}
    >
      <Plus size={15} />
      {t('shell.createLibrary')}
    </Button>
  )
}
