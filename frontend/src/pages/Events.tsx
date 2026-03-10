import { useEffect, useState } from 'react'
import { motion } from 'framer-motion'
import { Plus, Trash2, Calendar, MapPin, Clock } from 'lucide-react'
import { useAppStore, type Event } from '../store'
import { format } from 'date-fns'
import { zhCN } from 'date-fns/locale'

export default function EventsPage() {
  const { events, fetchEvents, createEvent, deleteEvent, familyId } = useAppStore()
  const [showForm, setShowForm] = useState(false)
  const [form, setForm] = useState({ title: '', description: '', start_time: '', end_time: '', category: '', location: '' })

  useEffect(() => { if (familyId) fetchEvents() }, [familyId, fetchEvents])

  const handleCreate = async () => {
    if (!form.title.trim() || !form.start_time) return
    await createEvent({
      title: form.title,
      description: form.description || undefined,
      start_time: new Date(form.start_time).toISOString(),
      end_time: form.end_time ? new Date(form.end_time).toISOString() : undefined,
      category: form.category || undefined,
      location: form.location || undefined,
    })
    setForm({ title: '', description: '', start_time: '', end_time: '', category: '', location: '' })
    setShowForm(false)
  }

  const categoryColors: Record<string, string> = {
    '生日': 'bg-pink-50 text-pink-700',
    '纪念日': 'bg-red-50 text-red-700',
    '医疗': 'bg-blue-50 text-blue-700',
    '出行': 'bg-green-50 text-green-700',
    '聚会': 'bg-purple-50 text-purple-700',
    '其他': 'bg-slate-50 text-slate-600',
  }

  const upcoming = events.filter(e => new Date(e.start_time) >= new Date())
  const past = events.filter(e => new Date(e.start_time) < new Date())

  return (
    <div className="p-6">
      <div className="flex items-center justify-between mb-6">
        <div>
          <h1 className="text-2xl font-bold text-slate-900">家庭日历</h1>
          <p className="text-sm text-slate-500 mt-0.5">时间维度 · {upcoming.length} 个即将到来</p>
        </div>
        <button onClick={() => setShowForm(true)} className="btn-primary">
          <Plus className="w-4 h-4" />添加事件
        </button>
      </div>

      {showForm && (
        <motion.div initial={{ opacity: 0, y: -8 }} animate={{ opacity: 1, y: 0 }} className="card border-purple-200 mb-6">
          <h3 className="font-semibold mb-4 text-slate-900">添加日历事件</h3>
          <div className="grid grid-cols-2 gap-3">
            <div className="col-span-2">
              <label className="label">事件标题 *</label>
              <input className="input-field" value={form.title} onChange={(e) => setForm(p => ({ ...p, title: e.target.value }))} placeholder="如：妈妈生日" />
            </div>
            <div>
              <label className="label">开始时间 *</label>
              <input className="input-field" type="datetime-local" value={form.start_time} onChange={(e) => setForm(p => ({ ...p, start_time: e.target.value }))} />
            </div>
            <div>
              <label className="label">结束时间</label>
              <input className="input-field" type="datetime-local" value={form.end_time} onChange={(e) => setForm(p => ({ ...p, end_time: e.target.value }))} />
            </div>
            <div>
              <label className="label">分类</label>
              <select className="input-field" value={form.category} onChange={(e) => setForm(p => ({ ...p, category: e.target.value }))}>
                <option value="">请选择</option>
                {['生日','纪念日','医疗','出行','聚会','其他'].map(c => <option key={c} value={c}>{c}</option>)}
              </select>
            </div>
            <div>
              <label className="label">地点</label>
              <input className="input-field" value={form.location} onChange={(e) => setForm(p => ({ ...p, location: e.target.value }))} placeholder="活动地点" />
            </div>
            <div className="col-span-2">
              <label className="label">描述</label>
              <input className="input-field" value={form.description} onChange={(e) => setForm(p => ({ ...p, description: e.target.value }))} placeholder="事件说明" />
            </div>
          </div>
          <div className="flex space-x-2 mt-4">
            <button onClick={handleCreate} className="btn-primary">确认添加</button>
            <button onClick={() => setShowForm(false)} className="btn-secondary">取消</button>
          </div>
        </motion.div>
      )}

      {events.length === 0 ? (
        <div className="card text-center py-16">
          <Calendar className="w-12 h-12 text-slate-200 mx-auto mb-3" />
          <p className="text-slate-400">暂无日程，点击右上角添加</p>
        </div>
      ) : (
        <div className="space-y-6">
          {upcoming.length > 0 && (
            <div>
              <h2 className="text-sm font-semibold text-slate-500 uppercase tracking-wide mb-3">即将到来</h2>
              <div className="space-y-2">
                {upcoming.map((ev: Event, i) => <EventCard key={ev.id} event={ev} index={i} onDelete={deleteEvent} categoryColors={categoryColors} />)}
              </div>
            </div>
          )}
          {past.length > 0 && (
            <div>
              <h2 className="text-sm font-semibold text-slate-400 uppercase tracking-wide mb-3">已过去</h2>
              <div className="space-y-2 opacity-60">
                {past.slice(0, 5).map((ev: Event, i) => <EventCard key={ev.id} event={ev} index={i} onDelete={deleteEvent} categoryColors={categoryColors} />)}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  )
}

function EventCard({ event, index, onDelete, categoryColors }: {
  event: Event; index: number; onDelete: (id: string) => void; categoryColors: Record<string, string>
}) {
  return (
    <motion.div
      initial={{ opacity: 0, x: -8 }}
      animate={{ opacity: 1, x: 0 }}
      transition={{ delay: index * 0.04 }}
      className="card flex items-center justify-between group"
    >
      <div className="flex items-center space-x-4">
        <div className="w-12 h-12 bg-purple-50 rounded-xl flex flex-col items-center justify-center">
          <span className="text-xs text-purple-400 font-medium">
            {format(new Date(event.start_time), 'MM月', { locale: zhCN })}
          </span>
          <span className="text-lg font-bold text-purple-700 leading-none">
            {format(new Date(event.start_time), 'd')}
          </span>
        </div>
        <div>
          <div className="font-medium text-slate-900 flex items-center space-x-2">
            <span>{event.title}</span>
            {event.category && (
              <span className={`text-xs px-2 py-0.5 rounded-full ${categoryColors[event.category] || 'bg-slate-100 text-slate-600'}`}>
                {event.category}
              </span>
            )}
          </div>
          <div className="flex items-center space-x-3 mt-0.5">
            <span className="flex items-center text-xs text-slate-400 space-x-1">
              <Clock className="w-3 h-3" />
              <span>{format(new Date(event.start_time), 'HH:mm')}</span>
            </span>
            {event.location && (
              <span className="flex items-center text-xs text-slate-400 space-x-1">
                <MapPin className="w-3 h-3" />
                <span>{event.location}</span>
              </span>
            )}
          </div>
        </div>
      </div>
      <button onClick={() => onDelete(event.id)} className="opacity-0 group-hover:opacity-100 text-red-400 hover:text-red-600 transition-all">
        <Trash2 className="w-4 h-4" />
      </button>
    </motion.div>
  )
}
