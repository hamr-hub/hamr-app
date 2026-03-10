import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom'
import Layout from './components/Layout'
import Dashboard from './pages/Dashboard'
import PeoplePage from './pages/People'
import EventsPage from './pages/Events'
import TasksPage from './pages/Tasks'
import ThingsPage from './pages/Things'
import SpacesPage from './pages/Spaces'

function RequireAuth({ children }: { children: React.ReactNode }) {
  const token = localStorage.getItem('access_token')
  if (!token) {
    window.location.href = import.meta.env.VITE_ACCOUNT_URL || 'http://localhost:3000/login'
    return null
  }
  return <>{children}</>
}

export default function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/" element={<RequireAuth><Layout /></RequireAuth>}>
          <Route index element={<Navigate to="/dashboard" replace />} />
          <Route path="dashboard" element={<Dashboard />} />
          <Route path="people" element={<PeoplePage />} />
          <Route path="events" element={<EventsPage />} />
          <Route path="tasks" element={<TasksPage />} />
          <Route path="things" element={<ThingsPage />} />
          <Route path="spaces" element={<SpacesPage />} />
        </Route>
      </Routes>
    </BrowserRouter>
  )
}
