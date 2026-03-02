import { createFileRoute } from '@tanstack/react-router';
import { useUIStore } from '../stores/uiStore';
import { useEffect } from 'react';
import { Moon, Sun } from 'lucide-react';
import { Button } from '../components/UI/button';

export const Route = createFileRoute('/settings')({
  component: SettingsPage,
});

function SettingsPage() {
  const { setActivePanel, theme, toggleTheme } = useUIStore();

  useEffect(() => {
    setActivePanel('settings');
  }, [setActivePanel]);

  return (
    <div className="max-w-2xl mx-auto">
      <h2 className="text-2xl font-bold mb-6">Settings</h2>
      
      <div className="space-y-6">
        <div className="rounded-lg border border-border bg-card p-6">
          <h3 className="text-lg font-semibold mb-4">Appearance</h3>
          
          <div className="space-y-4">
            <div>
              <label className="text-sm font-medium mb-2 block">Theme</label>
              <div className="flex gap-2">
                <Button
                  variant={theme === 'light' ? 'default' : 'outline'}
                  onClick={() => theme !== 'light' && toggleTheme()}
                  className="flex-1"
                >
                  <Sun className="mr-2 h-4 w-4" />
                  Light
                </Button>
                <Button
                  variant={theme === 'dark' ? 'default' : 'outline'}
                  onClick={() => theme !== 'dark' && toggleTheme()}
                  className="flex-1"
                >
                  <Moon className="mr-2 h-4 w-4" />
                  Dark
                </Button>
              </div>
            </div>
          </div>
        </div>

        <div className="rounded-lg border border-border bg-card p-6">
          <h3 className="text-lg font-semibold mb-4">Keyboard Shortcuts</h3>
          <div className="space-y-2 text-sm">
            <div className="flex justify-between py-2 border-b border-border">
              <span>Search</span>
              <kbd className="px-2 py-1 bg-muted rounded">Ctrl+K</kbd>
            </div>
            <div className="flex justify-between py-2 border-b border-border">
              <span>Toggle Sidebar</span>
              <kbd className="px-2 py-1 bg-muted rounded">Ctrl+B</kbd>
            </div>
            <div className="flex justify-between py-2 border-b border-border">
              <span>Go to Graph</span>
              <kbd className="px-2 py-1 bg-muted rounded">G</kbd>
            </div>
            <div className="flex justify-between py-2 border-b border-border">
              <span>Go to Codebases</span>
              <kbd className="px-2 py-1 bg-muted rounded">F</kbd>
            </div>
            <div className="flex justify-between py-2">
              <span>Close Sidebar</span>
              <kbd className="px-2 py-1 bg-muted rounded">Esc</kbd>
            </div>
          </div>
        </div>

        <div className="rounded-lg border border-border bg-card p-6">
          <h3 className="text-lg font-semibold mb-4">About</h3>
          <p className="text-sm text-muted-foreground">
            LeIndex Dashboard v0.1.0
          </p>
          <p className="text-sm text-muted-foreground mt-1">
            A modern interface for LeIndex code intelligence.
          </p>
        </div>
      </div>
    </div>
  );
}
