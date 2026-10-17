import { createElement, useLayoutEffect, useRef } from 'react';
import { mountControlRoom } from './control-room.js';

/** Optional React lifecycle bridge; React is a peer, never bundled in the core module. */
export function ControlRoom({ inventory, client, theme = 'inherit', tenantId, onIntent, className }) {
  const root = useRef(null);
  const mounted = useRef(null);
  const initial = useRef({ inventory, client, theme, tenantId, onIntent });
  useLayoutEffect(() => {
    mounted.current = mountControlRoom(root.current, initial.current);
    return () => { mounted.current?.destroy(); mounted.current = null; };
  }, []);
  useLayoutEffect(() => {
    mounted.current?.update({ inventory, client, theme, ...(tenantId ? { tenantId } : {}), onIntent });
  }, [inventory, client, theme, tenantId, onIntent]);
  return createElement('div', { ref:root, className });
}
