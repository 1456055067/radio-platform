import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from 'react';
import type { FlashbarProps } from '@cloudscape-design/components/flashbar';

type Item = FlashbarProps.MessageDefinition;
interface NotifyValue {
  items: Item[];
  notify: (type: 'success' | 'error' | 'info' | 'warning', content: ReactNode, header?: string) => void;
}

const Ctx = createContext<NotifyValue>({ items: [], notify: () => {} });

export const useNotify = () => useContext(Ctx).notify;
export const useNotifications = () => useContext(Ctx).items;

let seq = 0;

export function NotificationsProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<Item[]>([]);
  const notify = useCallback<NotifyValue['notify']>((type, content, header) => {
    const id = `n${++seq}`;
    const dismiss = () => setItems((xs) => xs.filter((x) => x.id !== id));
    setItems((xs) => [{ id, type, content, header, dismissible: true, onDismiss: dismiss }, ...xs].slice(0, 5));
    if (type === 'success' || type === 'info') setTimeout(dismiss, 6000);
  }, []);
  const value = useMemo(() => ({ items, notify }), [items, notify]);
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
