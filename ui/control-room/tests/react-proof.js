import { createElement, StrictMode, useState, useCallback } from 'react';
import { createRoot } from 'react-dom/client';
import { ControlRoom } from '../src/react.js';
import { applyScheme } from '../src/theme.js';
import { controlRoomScheme, controlRoomIdentity } from '../src/index.js';
import { sampleInventory } from '../demo/inventory.js';

const host = document.querySelector('[data-react-host]');
applyScheme(controlRoomScheme,{root:host,identity:controlRoomIdentity,radius:'8px'});
function App() {
  const [mounted,setMounted]=useState(true);
  const [count,setCount]=useState(0);
  const onIntent=useCallback(()=>setCount(value=>value+1),[]);
  return createElement('section',null,
    createElement('button',{'data-react-toggle':'',onClick:()=>setMounted(value=>!value)},mounted?'Unmount React module':'Mount React module'),
    createElement('span',{'data-react-count':''},String(count)),
    mounted?createElement(ControlRoom,{inventory:sampleInventory,onIntent,className:'react-room'}):null
  );
}
createRoot(host).render(createElement(StrictMode,null,createElement(App)));
