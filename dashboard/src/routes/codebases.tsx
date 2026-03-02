import { createFileRoute, Navigate } from '@tanstack/react-router';

export const Route = createFileRoute('/codebases')({
  component: () => <Navigate to="/" />,
});
