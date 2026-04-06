import { useNavigate } from 'react-router-dom';
import { Button } from '@/components/ui/button';
import { Home } from 'lucide-react';

export default function NotFoundPage() {
  const navigate = useNavigate();

  return (
    <div className="min-h-screen flex items-center justify-center bg-background ambient-bg">
      <div className="text-center animate-fade-in relative z-10">
        <div className="text-8xl font-black tracking-tighter mb-4" style={{
          background: 'linear-gradient(180deg, hsl(var(--foreground) / 0.15), hsl(var(--foreground) / 0.05))',
          WebkitBackgroundClip: 'text',
          WebkitTextFillColor: 'transparent',
        }}>404</div>
        <h1 className="text-lg font-bold tracking-tight">Page not found</h1>
        <p className="text-sm text-muted-foreground mt-2 mb-6">The page you're looking for doesn't exist.</p>
        <Button variant="outline" onClick={() => navigate('/dashboard')}>
          <Home className="h-4 w-4 mr-2" /> Back to Home
        </Button>
      </div>
    </div>
  );
}
