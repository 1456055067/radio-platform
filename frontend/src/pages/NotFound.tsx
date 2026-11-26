import Box from '@cloudscape-design/components/box';
import Header from '@cloudscape-design/components/header';
import Link from '@cloudscape-design/components/link';

export function NotFound() {
  return (
    <Box padding="l">
      <Header variant="h1">Page not found</Header>
      <Link href="/">Back to the dashboard</Link>
    </Box>
  );
}
