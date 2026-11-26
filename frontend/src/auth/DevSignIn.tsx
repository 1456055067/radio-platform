import Alert from '@cloudscape-design/components/alert';
import Box from '@cloudscape-design/components/box';
import Button from '@cloudscape-design/components/button';
import Container from '@cloudscape-design/components/container';
import Header from '@cloudscape-design/components/header';
import SpaceBetween from '@cloudscape-design/components/space-between';
import type { DevRole } from './session';

const ROLES: { role: DevRole; label: string; description: string }[] = [
  { role: 'admin', label: 'Administrator', description: 'Every permission, including enrollment, raw commands and the audit log.' },
  { role: 'operator', label: 'Operator', description: 'View scanners and events, and take control of a scanner.' },
  { role: 'viewer', label: 'Viewer', description: 'Read-only: scanners, telemetry and transmission history.' },
];

/** Role picker shown only by `radio-devstack` (auth mode `dev`). */
export function DevSignIn({ onChoose }: { onChoose: (r: DevRole) => void }) {
  return (
    <Box padding="xxl">
      <div style={{ maxWidth: 640, margin: '0 auto' }}>
        <SpaceBetween size="l">
          <Alert type="warning" header="Development stack">
            This console is connected to <code>radio-devstack</code>. Every scanner is a software simulator and all radio
            traffic is invented. Sign-in is disabled: pick a role to receive a test token.
          </Alert>
          <Container header={<Header variant="h1">Sign in as…</Header>}>
            <SpaceBetween size="m">
              {ROLES.map((r) => (
                <SpaceBetween key={r.role} size="xxs">
                  <Button variant={r.role === 'operator' ? 'primary' : 'normal'} onClick={() => onChoose(r.role)}>
                    {r.label}
                  </Button>
                  <Box variant="small" color="text-body-secondary">
                    {r.description}
                  </Box>
                </SpaceBetween>
              ))}
            </SpaceBetween>
          </Container>
        </SpaceBetween>
      </div>
    </Box>
  );
}
