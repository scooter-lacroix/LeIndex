import { useEffect, useCallback } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { useUIStore } from '../stores/uiStore';

export type ShortcutCallback = (event: KeyboardEvent) => void;

export interface Shortcut {
  key: string;
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
  meta?: boolean;
  callback: ShortcutCallback;
  preventDefault?: boolean;
}

export function useKeyboardShortcuts(shortcuts: Shortcut[]) {
  const handleKeyDown = useCallback(
    (event: KeyboardEvent) => {
      shortcuts.forEach((shortcut) => {
        const keyMatch = event.key.toLowerCase() === shortcut.key.toLowerCase();
        const ctrlMatch = !!shortcut.ctrl === (event.ctrlKey || event.metaKey);
        const shiftMatch = !!shortcut.shift === event.shiftKey;
        const altMatch = !!shortcut.alt === event.altKey;
        const metaMatch = !!shortcut.meta === event.metaKey;

        if (keyMatch && ctrlMatch && shiftMatch && altMatch && metaMatch) {
          if (shortcut.preventDefault !== false) {
            event.preventDefault();
          }
          shortcut.callback(event);
        }
      });
    },
    [shortcuts]
  );

  useEffect(() => {
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [handleKeyDown]);
}

export function useGlobalShortcuts() {
  const { setSidebarOpen } = useUIStore();
  const navigate = useNavigate();

  useKeyboardShortcuts([
    {
      key: 'k',
      ctrl: true,
      callback: () => navigate({ to: '/search' }),
    },
    {
      key: 'f',
      ctrl: true,
      shift: true,
      callback: () => navigate({ to: '/search' }),
    },
    {
      key: 'b',
      ctrl: true,
      callback: () => setSidebarOpen(true),
    },
    {
      key: 'g',
      callback: () => navigate({ to: '/graph' }),
    },
    {
      key: 'f',
      callback: () => navigate({ to: '/' }),
    },
    {
      key: 'Escape',
      callback: () => setSidebarOpen(false),
    },
  ]);
}
