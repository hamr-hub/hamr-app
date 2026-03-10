import { useEffect, useState } from 'react'
import { motion } from 'framer-motion'
import { Plus, Trash2, Package, MapPin } from 'lucide-react'
import { useAppStore, type Thing } from '../store'

const categoryIcons: Record<string, string> = {
  '家电': '🔌', '家具': '🪑', '食品': '🥫', '药品': '💊',
  '衣物': '👕', '书籍': '📚', '工具': '🔧', '其他': '📦',
}

export default function ThingsPage() {
  const { things, fetchThings, createThing, deleteThing, familyId } = useAppStore()
  const [showForm, setShowForm] = useState(false)
  const [categoryFilter, setCategoryFilter] = useState('全部')
  const [form, setForm] = useState({ name: '', category: '', location: '', quantity: '1', unit: '', notes: '', expiry_date: '' })

  useEffect(() => { if (familyId) fetchThings() }, [familyId, fetchThings])

  const handleCreate = async () => {
    if (!form.name.trim()) return
    await createThing({
      name: form.name, category: form.category || undefined,
      location: form.location || undefined, quantity: parseInt(form.quantity) || 1,
      unit: form.unit || undefined, notes: form.notes || undefined,
      expiry_date: form.expiry_date || undefined,
    })
    setForm({ name: '', category: '', location: '', quantity: '1', unit: '', notes: '', expiry_date: '' })
    setShowForm(false)
  }

  const categories = ['全部', ...Array.from(new Set(things.map(t => t.category || '其他').filter(Boolean)))]
  const filtered = categoryFilter === '全部' ? things : things.filter(t => (t.category || '其他') === categoryFilter)

  return (
    <div className="p-6">
      <div className="flex items-center justify-between mb-6">
        <div>
          <h1 className="text-2xl font-bold text-slate-900">家庭物品</h1>
          <p className="text-sm text-slate-500 mt-0.5">物品维度 · {things.length} 件物品</p>
        </div>
        <button onClick={() => setShowForm(true)} className="btn-primary">
          <Plus className="w-4 h-4" />登记物品
        </button>
      </div>

      <div className="flex space-x-2 mb-4 flex-wrap gap-y-2">
        {categories.map(c => (
          <button key={c} onClick={() => setCategoryFilter(c)}
            className={`px-3 py-1 text-sm rounded-lg font-medium transition-colors ${categoryFilter === c ? 'bg-primary-600 text-white' : 'text-slate-600 bg-slate-100 hover:bg-slate-200'}`}>
            {categoryIcons[c] || ''} {c}
          </button>
        ))}
      </div>

      {showForm && (
        <motion.div initial={{ opacity: 0, y: -8 }} animate={{ opacity: 1, y: 0 }} className="card border-pink-200 mb-6">
          <h3 className="font-semibold mb-4 text-slate-900">登记物品</h3>
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="label">物品名称 *</label>
              <input className="input-field" value={form.name} onChange={(e) => setForm(p => ({ ...p, name: e.target.value }))} placeholder="如：冰箱" />
            </div>
            <div>
              <label className="label">分类</label>
              <select className="input-field" value={form.category} onChange={(e) => setForm(p => ({ ...p, category: e.target.value }))}>
                <option value="">请选择</option>
                {Object.keys(categoryIcons).map(c => <option key={c} value={c}>{c}</option>)}
              </select>
            </div>
            <div>
              <label className="label">存放位置</label>
              <input className="input-field" value={form.location} onChange={(e) => setForm(p => ({ ...p, location: e.target.value }))} placeholder="如：客厅" />
            </div>
            <div className="flex space-x-2">
              <div className="flex-1">
                <label className="label">数量</label>
                <input className="input-field" type="number" min="1" value={form.quantity} onChange={(e) => setForm(p => ({ ...p, quantity: e.target.value }))} />
              </div>
              <div className="flex-1">
                <label className="label">单位</label>
                <input className="input-field" value={form.unit} onChange={(e) => setForm(p => ({ ...p, unit: e.target.value }))} placeholder="个/台" />
              </div>
            </div>
            <div>
              <label className="label">过期日期</label>
              <input className="input-field" type="date" value={form.expiry_date} onChange={(e) => setForm(p => ({ ...p, expiry_date: e.target.value }))} />
            </div>
            <div>
              <label className="label">备注</label>
              <input className="input-field" value={form.notes} onChange={(e) => setForm(p => ({ ...p, notes: e.target.value }))} placeholder="备注信息" />
            </div>
          </div>
          <div className="flex space-x-2 mt-4">
            <button onClick={handleCreate} className="btn-primary">确认登记</button>
            <button onClick={() => setShowForm(false)} className="btn-secondary">取消</button>
          </div>
        </motion.div>
      )}

      {filtered.length === 0 ? (
        <div className="card text-center py-16">
          <Package className="w-12 h-12 text-slate-200 mx-auto mb-3" />
          <p className="text-slate-400">暂无物品记录</p>
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3">
          {filtered.map((thing: Thing, i) => (
            <motion.div
              key={thing.id}
              initial={{ opacity: 0, y: 8 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ delay: i * 0.04 }}
              className="card group flex items-center justify-between"
            >
              <div className="flex items-center space-x-3">
                <span className="text-2xl">{categoryIcons[thing.category || ''] || '📦'}</span>
                <div>
                  <div className="font-medium text-slate-900 text-sm">{thing.name}</div>
                  <div className="flex items-center space-x-2 mt-0.5">
                    {thing.location && (
                      <span className="flex items-center text-xs text-slate-400 space-x-0.5">
                        <MapPin className="w-3 h-3" /><span>{thing.location}</span>
                      </span>
                    )}
                    <span className="text-xs text-slate-400">x{thing.quantity}{thing.unit || ''}</span>
                  </div>
                  {thing.expiry_date && (
                    <p className={`text-xs mt-0.5 ${new Date(thing.expiry_date) < new Date() ? 'text-red-500 font-medium' : 'text-slate-400'}`}>
                      {new Date(thing.expiry_date) < new Date() ? '⚠️ 已过期' : `过期：${thing.expiry_date}`}
                    </p>
                  )}
                </div>
              </div>
              <button onClick={() => deleteThing(thing.id)} className="opacity-0 group-hover:opacity-100 text-red-400 hover:text-red-600 transition-all">
                <Trash2 className="w-4 h-4" />
              </button>
            </motion.div>
          ))}
        </div>
      )}
    </div>
  )
}
