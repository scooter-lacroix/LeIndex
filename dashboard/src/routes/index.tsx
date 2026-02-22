import { createFileRoute } from '@tanstack/react-router';
import { CodebaseList } from '../components/Codebase/CodebaseList';
import { useUIStore } from '../stores/uiStore';
import { useEffect } from 'react';

export const Route = createFileRoute('/')({
  component: CodebasesPage,
});

function CodebasesPage() {
  const { setActivePanel } = useUIStore();

  useEffect(() => {
    setActivePanel('codebases');
  }, [setActivePanel]);

  return <CodebaseList />;
}
