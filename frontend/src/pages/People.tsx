import { useEffect, useState } from 'react'
import { motion } from 'framer-motion'
import { Plus, Trash2, User, Phone, Mail, Tag } from 'lucide-react'
import { useAppStore, type Person } from '../store'

export default function PeoplePage() {
  const { people, fetchPeople, createPerson, deletePerson, familyId } = useAppStore()
  const [showForm, setShowForm] = useState(false)
  const [form, setForm] = useState({ name: '', role: '', phone: '', email: '', birthday: '', notes: '' })

  useEffect(() => { if (familyId) fetchPeople() }, [familyId, fetchPeople])

  const handleCreate = async () => {
    if (!form.name.trim()) return
    await createPerson({ name: form.name, role: form.role || undefined, phone: form.phone || undefined, email: form.email || undefined, birthday: form.birthday || undefined, notes: form.notes || undefined })
    setForm({ name: '', role: '', phone: '', email: '', birthday: '', notes: '' })
    setShowForm(false)
  }

  const roleColors: Record<string, string> = {
    '父亲': 'bg-blue-50 text-blue-700',
    '母亲': 'bg-pink-50 text-pink-700',
    '子女': 'bg-green-50 text-green-700',
    '祖父': 'bg-amber-50 text-amber-700',
    '祖母': 'bg-amber-50 text-amber-700',
  }

  return (
    <div className="p-6">
      <div className="flex items-center justify-between mb-6">
        <div>
          <h1 className="text-2xl font-bold text-slate-900">家庭成员</h1>
          <p className="text-sm text-slate-500 mt-0.5">人员维度 · {people.length} 位成员</p>
        </div>
        <button onClick={() => setShowForm(true)} className="btn-primary">
          <Plus className="w-4 h-4" />添加成员
        </button>
      </div>

      {showForm && (
        <motion.div initial={{ opacity: 0, y: -8 }} animate={{ opacity: 1, y: 0 }} className="card border-primary-200 mb-6">
          <h3 className="font-semibold mb-4 text-slate-900">添加家庭成员</h3>
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="label">姓名 *</label>
              <input className="input-field" value={form.name} onChange={(e) => setForm(p => ({ ...p, name: e.target.value }))} placeholder="成员姓名" />
            </div>
            <div>
              <label className="label">角色</label>
              <select className="input-field" value={form.role} onChange={(e) => setForm(p => ({ ...p, role: e.target.value }))}>
                <option value="">请选择</option>
                {['父亲','母亲','子女','祖父','祖母','其他'].map(r => <option key={r} value={r}>{r}</option>)}
              </select>
            </div>
            <div>
              <label className="label">手机</label>
              <input className="input-field" value={form.phone} onChange={(e) => setForm(p => ({ ...p, phone: e.target.value }))} placeholder="手机号码" />
            </div>
            <div>
              <label className="label">邮箱</label>
              <input className="input-field" value={form.email} onChange={(e) => setForm(p => ({ ...p, email: e.target.value }))} placeholder="邮箱地址" type="email" />
            </div>
            <div>
              <label className="label">生日</label>
              <input className="input-field" value={form.birthday} onChange={(e) => setForm(p => ({ ...p, birthday: e.target.value }))} type="date" />
            </div>
            <div>
              <label className="label">备注</label>
              <input className="input-field" value={form.notes} onChange={(e) => setForm(p => ({ ...p, notes: e.target.value }))} placeholder="备注信息" />
            </div>
          </div>
          <div className="flex space-x-2 mt-4">
            <button onClick={handleCreate} className="btn-primary">确认添加</button>
            <button onClick={() => setShowForm(false)} className="btn-secondary">取消</button>
          </div>
        </motion.div>
      )}

      {people.length === 0 ? (
        <div className="card text-center py-16">
          <User className="w-12 h-12 text-slate-200 mx-auto mb-3" />
          <p className="text-slate-400">暂无成员，点击右上角添加</p>
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
          {people.map((person: Person, i) => (
            <motion.div
              key={person.id}
              initial={{ opacity: 0, y: 8 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ delay: i * 0.05 }}
              className="card group"
            >
              <div className="flex items-start justify-between">
                <div className="flex items-center space-x-3">
                  <div className="w-10 h-10 bg-primary-100 rounded-full flex items-center justify-center font-bold text-primary-700">
                    {person.name.charAt(0)}
                  </div>
                  <div>
                    <div className="font-semibold text-slate-900">{person.name}</div>
                    {person.role && (
                      <span className={`text-xs px-2 py-0.5 rounded-full ${roleColors[person.role] || 'bg-slate-100 text-slate-600'}`}>
                        {person.role}
                      </span>
                    )}
                  </div>
                </div>
                <button onClick={() => deletePerson(person.id)} className="opacity-0 group-hover:opacity-100 text-red-400 hover:text-red-600 transition-all">
                  <Trash2 className="w-4 h-4" />
                </button>
              </div>
              <div className="mt-3 space-y-1.5">
                {person.phone && <div className="flex items-center space-x-2 text-xs text-slate-500"><Phone className="w-3 h-3" /><span>{person.phone}</span></div>}
                {person.email && <div className="flex items-center space-x-2 text-xs text-slate-500"><Mail className="w-3 h-3" /><span>{person.email}</span></div>}
                {person.birthday && <div className="flex items-center space-x-2 text-xs text-slate-500"><Tag className="w-3 h-3" /><span>生日：{person.birthday}</span></div>}
                {person.notes && <p className="text-xs text-slate-400 mt-1">{person.notes}</p>}
              </div>
            </motion.div>
          ))}
        </div>
      )}
    </div>
  )
}
