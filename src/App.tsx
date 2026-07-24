import { Layout } from './components/layout'
import { ThemeProvider } from './lib/theme-provider'
import './App.css'

function App() {
  return (
    <ThemeProvider defaultTheme="system" storageKey="astro-editor-theme">
      <Layout />
    </ThemeProvider>
  )
}

export default App
