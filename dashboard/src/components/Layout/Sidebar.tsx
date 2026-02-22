import { Link } from '@tanstack/react-router';
import { 
  Database, 
  Share2, 
  Search, 
  Settings, 
  ChevronLeft, 
  RefreshCw 
} from 'lucide-react';
import { Button } from '../UI/button';
import { useUIStore } from '../../stores/uiStore';
import { useRefreshCodebases } from '../../hooks/useCodebases';
import { LoadingSpinner } from '../LoadingSpinner';
import { cn } from '../../lib/utils';

const navItems = [
  { to: '/', icon: Database, label: 'Codebases' },
  { to: '/graph', icon: Share2, label: 'Graph' },
  { to: '/search', icon: Search, label: 'Search' },
  { to: '/settings', icon: Settings, label: 'Settings' },
];

export function Sidebar() {
  const { sidebarOpen, setSidebarOpen, activePanel } = useUIStore();
  const refreshMutation = useRefreshCodebases();

  return (
    <aside
      className={cn(
        'fixed left-0 top-0 z-40 h-screen bg-card border-r border-border transition-all duration-300',
        sidebarOpen ? 'w-64' : 'w-0 overflow-hidden'
      )}
    >
      <div className="flex h-full flex-col">
        <div className="flex h-16 items-center justify-between border-b border-border px-4">
          <h1 className="text-lg font-bold">LeIndex</h1>
          <Button
            variant="ghost"
            size="icon"
            onClick={() => setSidebarOpen(false)}
            className="h-8 w-8"
          >
            <ChevronLeft className="h-4 w-4" />
          </Button>
        </div>

        <nav className="flex-1 space-y-1 p-4">
          {navItems.map((item) => (
            <Link
              key={item.to}
              to={item.to}
              className={cn(
                'flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition-colors',
                activePanel === item.label.toLowerCase()
                  ? 'bg-primary text-primary-foreground'
                  : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground'
              )}
            >
              <item.icon className="h-4 w-4" />
              {item.label}
            </Link>
          ))}
        </nav>

        <div className="border-t border-border p-4">
          <Button
            variant="outline"
            className="w-full"
            onClick={() => refreshMutation.mutate()}
            disabled={refreshMutation.isPending}
          >
            {refreshMutation.isPending ? (
              <LoadingSpinner size="sm" className="mr-2" />
            ) : (
              <RefreshCw className="mr-2 h-4 w-4" />
            )}
            Refresh Indexes
          </Button>
        </div>
      </div>
    </aside>
  );
}
