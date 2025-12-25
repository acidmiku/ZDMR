import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import { AppWithContext } from './App.tsx'

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <AppWithContext />
  </StrictMode>,
)
