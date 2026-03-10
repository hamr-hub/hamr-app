import { create } from 'zustand'
import { api } from '../api'

export interface DashboardStats {
  family_id: string
  people_count: number
  upcoming_events: number
  pending_tasks: number
  things_count: number
  spaces_count: number
}

export interface Person {
  id: string; family_id: string; name: string; role?: string
  birthday?: string; phone?: string; email?: string
  notes?: string; tags?: string[]; created_at: string
}

export interface Event {
  id: string; family_id: string; title: string; description?: string
  start_time: string; end_time?: string; all_day: boolean
  category?: string; location?: string; created_at: string
}

export interface Task {
  id: string; family_id: string; title: string; description?: string
  status: string; priority: string; due_date?: string
  assigned_to?: string; tags?: string[]; is_milestone: boolean
  completed_at?: string; created_at: string
}

export interface Thing {
  id: string; family_id: string; name: string; category?: string
  description?: string; location?: string; quantity: number
  unit?: string; status: string; expiry_date?: string
  notes?: string; tags?: string[]; created_at: string
}

export interface Space {
  id: string; family_id: string; name: string; type?: string
  description?: string; icon?: string; notes?: string; created_at: string
}

interface AppStore {
  familyId: string | null
  setFamilyId: (id: string) => void
  stats: DashboardStats | null
  people: Person[]
  events: Event[]
  tasks: Task[]
  things: Thing[]
  spaces: Space[]
  loading: boolean
  fetchStats: () => Promise<void>
  fetchPeople: () => Promise<void>
  fetchEvents: () => Promise<void>
  fetchTasks: () => Promise<void>
  fetchThings: () => Promise<void>
  fetchSpaces: () => Promise<void>
  createPerson: (data: Partial<Person>) => Promise<void>
  updatePerson: (id: string, data: Partial<Person>) => Promise<void>
  deletePerson: (id: string) => Promise<void>
  createEvent: (data: Partial<Event>) => Promise<void>
  deleteEvent: (id: string) => Promise<void>
  createTask: (data: Partial<Task>) => Promise<void>
  updateTask: (id: string, data: Partial<Task>) => Promise<void>
  deleteTask: (id: string) => Promise<void>
  createThing: (data: Partial<Thing>) => Promise<void>
  deleteThing: (id: string) => Promise<void>
  createSpace: (data: Partial<Space>) => Promise<void>
  deleteSpace: (id: string) => Promise<void>
}

export const useAppStore = create<AppStore>((set, get) => ({
  familyId: localStorage.getItem('family_id'),
  setFamilyId: (id) => {
    localStorage.setItem('family_id', id)
    set({ familyId: id })
  },
  stats: null, people: [], events: [], tasks: [], things: [], spaces: [],
  loading: false,

  fetchStats: async () => {
    const { familyId } = get(); if (!familyId) return
    const { data } = await api.get<DashboardStats>(`/dashboard?family_id=${familyId}`)
    set({ stats: data })
  },
  fetchPeople: async () => {
    const { familyId } = get(); if (!familyId) return
    const { data } = await api.get<Person[]>(`/people?family_id=${familyId}`)
    set({ people: data })
  },
  fetchEvents: async () => {
    const { familyId } = get(); if (!familyId) return
    const { data } = await api.get<Event[]>(`/events?family_id=${familyId}`)
    set({ events: data })
  },
  fetchTasks: async () => {
    const { familyId } = get(); if (!familyId) return
    const { data } = await api.get<Task[]>(`/tasks?family_id=${familyId}`)
    set({ tasks: data })
  },
  fetchThings: async () => {
    const { familyId } = get(); if (!familyId) return
    const { data } = await api.get<Thing[]>(`/things?family_id=${familyId}`)
    set({ things: data })
  },
  fetchSpaces: async () => {
    const { familyId } = get(); if (!familyId) return
    const { data } = await api.get<Space[]>(`/spaces?family_id=${familyId}`)
    set({ spaces: data })
  },

  createPerson: async (data) => {
    const { familyId } = get(); if (!familyId) return
    const { data: row } = await api.post<Person>('/people', { ...data, family_id: familyId })
    set((s) => ({ people: [...s.people, row] }))
  },
  updatePerson: async (id, data) => {
    const { data: row } = await api.put<Person>(`/people/${id}`, data)
    set((s) => ({ people: s.people.map((p) => p.id === id ? row : p) }))
  },
  deletePerson: async (id) => {
    await api.delete(`/people/${id}`)
    set((s) => ({ people: s.people.filter((p) => p.id !== id) }))
  },

  createEvent: async (data) => {
    const { familyId } = get(); if (!familyId) return
    const { data: row } = await api.post<Event>('/events', { ...data, family_id: familyId })
    set((s) => ({ events: [...s.events, row] }))
  },
  deleteEvent: async (id) => {
    await api.delete(`/events/${id}`)
    set((s) => ({ events: s.events.filter((e) => e.id !== id) }))
  },

  createTask: async (data) => {
    const { familyId } = get(); if (!familyId) return
    const { data: row } = await api.post<Task>('/tasks', { ...data, family_id: familyId })
    set((s) => ({ tasks: [...s.tasks, row] }))
  },
  updateTask: async (id, data) => {
    const { data: row } = await api.put<Task>(`/tasks/${id}`, data)
    set((s) => ({ tasks: s.tasks.map((t) => t.id === id ? row : t) }))
  },
  deleteTask: async (id) => {
    await api.delete(`/tasks/${id}`)
    set((s) => ({ tasks: s.tasks.filter((t) => t.id !== id) }))
  },

  createThing: async (data) => {
    const { familyId } = get(); if (!familyId) return
    const { data: row } = await api.post<Thing>('/things', { ...data, family_id: familyId })
    set((s) => ({ things: [...s.things, row] }))
  },
  deleteThing: async (id) => {
    await api.delete(`/things/${id}`)
    set((s) => ({ things: s.things.filter((t) => t.id !== id) }))
  },

  createSpace: async (data) => {
    const { familyId } = get(); if (!familyId) return
    const { data: row } = await api.post<Space>('/spaces', { ...data, family_id: familyId })
    set((s) => ({ spaces: [...s.spaces, row] }))
  },
  deleteSpace: async (id) => {
    await api.delete(`/spaces/${id}`)
    set((s) => ({ spaces: s.spaces.filter((sp) => sp.id !== id) }))
  },
}))
