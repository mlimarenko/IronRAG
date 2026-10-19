import { ArrowLeft, Compass } from 'lucide-react'
import { Link } from 'react-router-dom'
import { useTranslation } from 'react-i18next'
import { Button } from '@/shared/ui'

export default function NotFoundPage() {
  const { t } = useTranslation()
  return (
    <div className="not-found">
      <Compass size={34} />
      <span>404</span>
      <h1>{t('notFound.title', { productName: getBranding().displayName })}</h1>
      <p>{t('notFound.body')}</p>
      <Button variant="primary">
        <Link to="/dashboard">
          <ArrowLeft size={15} />
          {t('notFound.home')}
        </Link>
      </Button>
    </div>
  )
}
import { getBranding } from './branding'
