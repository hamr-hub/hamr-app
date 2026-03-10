import { useEffect, useState } from 'react'
import { motion } from 'framer-motion'
import { Plus, CheckCircle2, Circle, Trash2, Flag } from 'lucide-react'
import { useAppStore, type Task } from '../store'

const priorityConfig = {
  urgent: { label: '紧急', color: 'text-red-600 bg-red-50' },
  high: { label: '高', color: 'text-orange-600 bg-orange-50' },
  medium: { label: '中', color: 'text-yellow-600 bg-yellow-50' },
  low: { label: '低', color: 'text-slate-500 bg-slate-50' },
}


export default function TasksPage() {
  const { tasks, fetchTasks, createTask, updateTask, deleteTask, familyId } = useAppStore()
  const [showForm, setShowForm] = useState(false)
  const [filter, setFilter] = useState<'all' | 'todo' | 'done'>('all')
  const [form, setForm] = useState({ title: '', description: '', priority: 'medium', due_date: '', is_milestone: false })

  useEffect(() => { if (familyId) fetchTasks() }, [familyId, fetchTasks])

  const handleCreate = async () => {
    if (!form.title.trim()) return
    await createTask({
      title: form.title,
      description: form.description || undefined,
      priority: form.priority,
      due_date: form.due_date ? new Date(form.due_date).toISOString() : undefined,
      is_milestone: form.is_milestone,
    })
    setForm({ title: '', description: '', priority: 'medium', due_date: '', is_milestone: false })
    setShowForm(false)
  }

  const toggleDone = async (task: Task) => {
    const newStatus = task.status === 'done' ? 'todo' : 'done'
    await updateTask(task.id, { status: newStatus })
  }

  const filtered = tasks.filter(t => {
    if (filter === 'todo') return t.status !== 'done' && t.status !== 'cancelled'
    if (filter === 'done') return t.status === 'done'
    return true
  })

  const todo = filtered.filter(t => t.status !== 'done' && t.status !== 'cancelled')
  const done = filtered.filter(t => t.status === 'done')

  return (
    <div className="p-6">
      <div className="flex items-center justify-between mb-6">
        <div>
          <h1 className="text-2xl font-bold text-slate-900">家庭事务</h1>
          <p className="text-sm text-slate-500 mt-0.5">事务维度 · {todo.length} 项待处理</p>
        </div>
        <button onClick={() => setShowForm(true)} className="btn-primary">
          <Plus className="w-4 h-4" />添加事务
        </button>
      </div>

      <div className="flex space-x-1 mb-4">
        {[['all','全部'],['todo','待办'],['done','已完成']].map(([v, l]) => (
          <button key={v} onClick={() => setFilter(v as 'all' | 'todo' | 'done')}
            className={`px-3 py-1.5 text-sm rounded-lg font-medium transition-colors ${filter === v ? 'bg-primary-600 text-white' : 'text-slate-600 hover:bg-slate-100'}`}>
            {l}
          </button>
        ))}
      </div>

      {showForm && (
        <motion.div initial={{ opacity: 0, y: -8 }} animate={{ opacity: 1, y: 0 }} className="card border-orange-200 mb-6">
          <h3 className="font-semibold mb-4 text-slate-900">添加事务</h3>
          <div className="grid grid-cols-2 gap-3">
            <div className="col-span-2">
              <label className="label">标题 *</label>
              <input className="input-field" value={form.title} onChange={(e) => setForm(p => ({ ...p, title: e.target.value }))} placeholder="事务名称" />
            </div>
            <div>
              <label className="label">优先级</label>
              <select className="input-field" value={form.priority} onChange={(e) => setForm(p => ({ ...p, priority: e.target.value }))}>
                {Object.entries(priorityConfig).map(([k, v]) => <option key={k} value={k}>{v.label}</option>)}
              </select>
            </div>
            <div>
              <label className="label">截止日期</label>
              <input className="input-field" type="datetime-local" value={form.due_date} onChange={(e) => setForm(p => ({ ...p, due_date: e.target.value }))} />
            </div>
            <div className="col-span-2">
              <label className="label">描述</label>
              <input className="input-field" value={form.description} onChange={(e) => setForm(p => ({ ...p, description: e.target.value }))} placeholder="详细说明" />
            </div>
            <div className="col-span-2 flex items-center space-x-2">
              <input type="checkbox" id="milestone" checked={form.is_milestone} onChange={(e) => setForm(p => ({ ...p, is_milestone: e.target.checked }))} className="w-4 h-4" />
              <label htmlFor="milestone" className="text-sm text-slate-700">标记为里程碑</label>
            </div>
          </div>
          <div className="flex space-x-2 mt-4">
            <button onClick={handleCreate} className="btn-primary">确认添加</button>
            <button onClick={() => setShowForm(false)} className="btn-secondary">取消</button>
          </div>
        </motion.div>
      )}

      {filtered.length === 0 ? (
        <div className="card text-center py-16">
          <CheckCircle2 className="w-12 h-12 text-slate-200 mx-auto mb-3" />
          <p className="text-slate-400">暂无事务</p>
        </div>
      ) : (
        <div className="space-y-6">
          {todo.length > 0 && (
            <div>
              <h2 className="text-xs font-semibold text-slate-400 uppercase tracking-wide mb-2">待处理 ({todo.length})</h2>
              <div className="space-y-2">
                {todo.map((task: Task, i) => <TaskCard key={task.id} task={task} index={i} onToggle={toggleDone} onDelete={deleteTask} />)}
              </div>
            </div>
          )}
          {done.length > 0 && (
            <div>
              <h2 className="text-xs font-semibold text-slate-400 uppercase tracking-wide mb-2">已完成 ({done.length})</h2>
              <div className="space-y-2 opacity-60">
                {done.map((task: Task, i) => <TaskCard key={task.id} task={task} index={i} onToggle={toggleDone} onDelete={deleteTask} />)}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  )
}

function TaskCard({ task, index, onToggle, onDelete }: {
  task: Task; index: number; onToggle: (t: Task) => void; onDelete: (id: string) => void
}) {
  const p = priorityConfig[task.priority as keyof typeof priorityConfig] || priorityConfig.medium
  const done = task.status === 'done'
  return (
    <motion.div
      initial={{ opacity: 0, x: -8 }}
      animate={{ opacity: 1, x: 0 }}
      transition={{ delay: index * 0.04 }}
      className="card flex items-center space-x-3 group"
    >
      <button onClick={() => onToggle(task)} className="shrink-0">
        {done
          ? <CheckCircle2 className="w-5 h-5 text-green-500" />
          : <Circle className="w-5 h-5 text-slate-300 hover:text-primary-500 transition-colors" />}
      </button>
      <div className="flex-1 min-w-0">
        <div className={`font-medium text-sm flex items-center space-x-2 ${done ? 'line-through text-slate-400' : 'text-slate-900'}`}>
          {task.is_milestone && <Flag className="w-3 h-3 text-amber-500 shrink-0" />}
          <span className="truncate">{task.title}</span>
          <span className={`text-xs px-1.5 py-0.5 rounded-full ${p.color} shrink-0`}>{p.label}</span>
        </div>
        {task.description && <p className="text-xs text-slate-400 mt-0.5 truncate">{task.description}</p>}
        {task.due_date && (
          <p className={`text-xs mt-0.5 ${new Date(task.due_date) < new Date() && !done ? 'text-red-500' : 'text-slate-400'}`}>
            截止：{new Date(task.due_date).toLocaleDateString('zh-CN')}
          </p>
        )}
      </div>
      <button onClick={() => onDelete(task.id)} className="opacity-0 group-hover:opacity-100 text-red-400 hover:text-red-600 transition-all shrink-0">
        <Trash2 className="w-4 h-4" />
      </button>
    </motion.div>
  )
}
