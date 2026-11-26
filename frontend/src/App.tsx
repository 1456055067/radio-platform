import { useMemo } from 'react';
import { Route, Routes, useLocation, useNavigate } from 'react-router';
import AppLayout from '@cloudscape-design/components/app-layout';
import Badge from '@cloudscape-design/components/badge';
import Flashbar from '@cloudscape-design/components/flashbar';
import SideNavigation, { type SideNavigationProps } from '@cloudscape-design/components/side-navigation';
import StatusIndicator from '@cloudscape-design/components/status-indicator';
import TopNavigation from '@cloudscape-design/components/top-navigation';
import { useAuth } from './auth/AuthContext';
import { useLive } from './hooks/LiveContext';
import { useNotifications } from './hooks/Notifications';
import { Dashboard } from './pages/Dashboard';
import { Scanners } from './pages/Scanners';
import { Sites } from './pages/Sites';
import { VirtualScanner } from './pages/VirtualScanner';
import { Transmissions } from './pages/Transmissions';
import { Events } from './pages/Events';
import { Audit } from './pages/Audit';
import { Analytics } from './pages/Analytics';
import { NotFound } from './pages/NotFound';

function LiveBadge() {
  const { state } = useLive();
  switch (state) {
    case 'open':
      return <StatusIndicator type="success">Live</StatusIndicator>;
    case 'connecting':
      return <StatusIndicator type="pending">Connecting</StatusIndicator>;
    case 'expired':
      return <StatusIndicator type="warning">Session expired</StatusIndicator>;
    default:
      return <StatusIndicator type="error">Live stream down</StatusIndicator>;
  }
}

export function App() {
  const { me, session, can } = useAuth();
  const navigate = useNavigate();
  const location = useLocation();
  const notifications = useNotifications();

  const nav = useMemo(() => {
    const items: SideNavigationProps.Item[] = [
      { type: 'link', text: 'Dashboard', href: '/' },
      { type: 'link', text: 'Scanners', href: '/scanners' },
      { type: 'link', text: 'Sites and agents', href: '/sites' },
      { type: 'divider' },
    ];
    if (can('event_view')) {
      items.push({ type: 'link', text: 'Transmissions', href: '/transmissions' });
      items.push({ type: 'link', text: 'Analytics', href: '/analytics' });
      items.push({ type: 'link', text: 'Events', href: '/events' });
    }
    if (can('audit_view')) items.push({ type: 'link', text: 'Audit log', href: '/audit' });
    return items;
  }, [can]);

  const active = '/' + (location.pathname.split('/')[1] ?? '');

  return (
    <>
      <div id="top-nav" style={{ position: 'sticky', top: 0, zIndex: 1002 }}>
        <TopNavigation
          identity={{
            href: '/',
            title: 'Radio Platform',
            onFollow: (e) => {
              e.preventDefault();
              navigate('/');
            },
          }}
          utilities={[
            {
              type: 'menu-dropdown',
              text: me.name ?? me.subject,
              description: session.mode === 'dev' ? `${me.subject} · simulated dev stack` : me.subject,
              iconName: 'user-profile',
              items: [{ id: 'signout', text: 'Sign out' }],
              onItemClick: (e) => {
                if (e.detail.id === 'signout') void session.signOut();
              },
            },
          ]}
        />
      </div>
      <AppLayout
        headerSelector="#top-nav"
        toolsHide
        notifications={<Flashbar items={notifications} stackItems />}
        navigation={
          <SideNavigation
            header={{ text: 'Console', href: '/' }}
            activeHref={active === '/' ? '/' : active}
            items={nav}
            itemsControl={
              <span>
                <LiveBadge /> {session.mode === 'dev' && <Badge color="red">Simulated</Badge>}
              </span>
            }
            onFollow={(e) => {
              if (!e.detail.external) {
                e.preventDefault();
                navigate(e.detail.href);
              }
            }}
          />
        }
        content={
          <Routes>
            <Route path="/" element={<Dashboard />} />
            <Route path="/scanners" element={<Scanners />} />
            <Route path="/scanners/:id" element={<VirtualScanner />} />
            <Route path="/sites" element={<Sites />} />
            <Route path="/transmissions" element={<Transmissions />} />
            <Route path="/analytics" element={<Analytics />} />
            <Route path="/events" element={<Events />} />
            <Route path="/audit" element={<Audit />} />
            <Route path="*" element={<NotFound />} />
          </Routes>
        }
      />
    </>
  );
}
