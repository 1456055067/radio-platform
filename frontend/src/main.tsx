import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { BrowserRouter } from 'react-router';
import { I18nProvider } from '@cloudscape-design/components/i18n';
import messages from '@cloudscape-design/components/i18n/messages/all.en';
import '@cloudscape-design/global-styles/index.css';
import './styles.css';
import { App } from './App';
import { AuthProvider } from './auth/AuthContext';
import { DirectoryProvider } from './hooks/Directory';
import { LiveProvider } from './hooks/LiveContext';
import { NotificationsProvider } from './hooks/Notifications';

const root = document.getElementById('root');
if (root) {
  createRoot(root).render(
    <StrictMode>
      <I18nProvider locale="en" messages={[messages]}>
      <AuthProvider>
        <BrowserRouter>
          <NotificationsProvider>
            <LiveProvider>
              <DirectoryProvider>
                <App />
              </DirectoryProvider>
            </LiveProvider>
          </NotificationsProvider>
        </BrowserRouter>
      </AuthProvider>
      </I18nProvider>
    </StrictMode>,
  );
}
