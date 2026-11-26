import { Component, Suspense, type ReactNode } from 'react';
import Alert from '@cloudscape-design/components/alert';
import Box from '@cloudscape-design/components/box';
import Button from '@cloudscape-design/components/button';
import Spinner from '@cloudscape-design/components/spinner';

interface State {
  error: Error | null;
}

/**
 * Catches a page that fails to load or render. Pages are loaded on demand,
 * so the usual cause is a redeploy: this tab still runs the previous build
 * and its chunks are gone (the gateway answers 404 for them).
 */
class Boundary extends Component<{ children: ReactNode }, State> {
  override state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  override render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    const chunk = /dynamically imported module|Importing a module script failed|error loading dynamically|Failed to fetch/i.test(error.message);
    return (
      <Alert
        type="error"
        header={chunk ? 'This page could not be loaded' : 'This page failed'}
        action={<Button onClick={() => window.location.reload()}>Reload</Button>}
      >
        {chunk ? 'The console may have been updated since this tab opened. Reload to get the current version.' : error.message}
      </Alert>
    );
  }
}

function PageLoading() {
  return (
    <Box textAlign="center" padding="xxl">
      <Spinner size="large" />
    </Box>
  );
}

/** Suspense and error boundary for one route; `resetKey` clears an error on navigation. */
export function PageBoundary({ resetKey, children }: { resetKey: string; children: ReactNode }) {
  return (
    <Boundary key={resetKey}>
      <Suspense fallback={<PageLoading />}>{children}</Suspense>
    </Boundary>
  );
}
