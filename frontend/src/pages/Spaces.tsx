import { useEffect, useState } from 'react'
import { motion } from 'framer-motion'
import { Plus, Trash2, Home } from 'lucide-react'
import { useAppStore, type Space } from '../store'

const spaceIcons: Record<string, string> = {
  '客厅': '🛋️', '卧室': '🛏️', '厨房': '🍳', '卫生间': '🚿',
  '书房': '📖', '儿童房': '🧸', '车库': '🚗', '阳台': '🌿', '其他': '🏠',
}

export default function SpacesPage() {
  const { spaces, fetchSpaces, createSpace, deleteSpace, familyId } = useAppStore()
  const [showForm, setShowForm] = useState(false)
  const [form, setForm] = useState({ name: '', type: '', description: '', icon: '', notes: '' })

  useEffect(() => { if (familyId) fetchSpaces() }, [familyId, fetchSpaces])

  const handleCreate = async () => {
    if (!form.name.trim()) return
    await createSpace({
      name: form.name, type: form.type || undefined,
      description: form.description || undefined,
      icon: form.icon || undefined, notes: form.notes || undefined,
    })
    setForm({ name: '', type: '', description: '', icon: '', notes: '' })
    setShowForm(false)
  }

  return (
    <div className="p-6">
      <div className="flex items-center justify-between mb-6">
        <div>
          <h1 className="text-2xl font-bold text-slate-900">生活空间</h1>
          <p className="text-sm text-slate-500 mt-0.5">环境维度 · {spaces.length} 个空间</p>
        </div>
        <button onClick={() => setShowForm(true)} className="btn-primary">
          <Plus className="w-4 h-4" />添加空间
        </button>
      </div>

      {showForm && (
        <motion.div initial={{ opacity: 0, y: -8 }} animate={{ opacity: 1, y: 0 }} className="card border-green-200 mb-6">
          <h3 className="font-semibold mb-4 text-slate-900">添加空间</h3>
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="label">空间名称 *</label>
              <input className="input-field" value={form.name} onChange={(e) => setForm(p => ({ ...p, name: e.target.value }))} placeholder="如：主卧" />
            </div>
            <div>
              <label className="label">空间类型</label>
              <select className="input-field" value={form.type} onChange={(e) => setForm(p => ({ ...p, type: e.target.value }))}>
                <option value="">请选择</option>
                {Object.keys(spaceIcons).map(t => <option key={t} value={t}>{t}</option>)}
              </select>
            </div>
            <div className="col-span-2">
              <label className="label">描述</label>
              <input className="input-field" value={form.description} onChange={(e) => setForm(p => ({ ...p, description: e.target.value }))} placeholder="空间说明" />
            </div>
          </div>
          <div className="flex space-x-2 mt-4">
            <button onClick={handleCreate} className="btn-primary">确认添加</button>
            <button onClick={() => setShowForm(false)} className="btn-secondary">取消</button>
          </div>
        </motion.div>
      )}

      {spaces.length === 0 ? (
        <div className="card text-center py-16">
          <Home className="w-12 h-12 text-slate-200 mx-auto mb-3" />
          <p className="text-slate-400">暂无空间记录</p>
        </div>
      ) : (
        <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-4 gap-4">
          {spaces.map((space: Space, i) => (
            <motion.div
              key={space.id}
              initial={{ opacity: 0, scale: 0.95 }}
              animate={{ opacity: 1, scale: 1 }}
              transition={{ delay: i * 0.05 }}
              className="card group text-center hover:shadow-md transition-shadow"
            >
              <div className="text-4xl mb-2">{spaceIcons[space.type || ''] || space.icon || '🏠'}</div>
              <div className="font-semibold text-slate-900 text-sm">{space.name}</div>
              {space.type && <div className="text-xs text-slate-400 mt-0.5">{space.type}</div>}
              {space.description && <p className="text-xs text-slate-400 mt-1 line-clamp-2">{space.description}</p>}
              <button
                onClick={() => deleteSpace(space.id)}
                className="opacity-0 group-hover:opacity-100 mt-2 text-red-400 hover:text-red-600 transition-all"
              >
                <Trash2 className="w-4 h-4 mx-auto" />
              </button>
            </motion.div>
          ))}
        </div>
      )}
    </div>
  )
}
