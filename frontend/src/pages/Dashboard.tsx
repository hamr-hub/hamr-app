import { useEffect } from 'react'
import { motion } from 'framer-motion'
import { Users, Calendar, CheckSquare, Package, Home, TrendingUp } from 'lucide-react'
import { useAppStore } from '../store'

export default function Dashboard() {
  const { stats, fetchStats, familyId } = useAppStore()

  useEffect(() => {
    if (familyId) fetchStats()
  }, [familyId, fetchStats])

  if (!familyId) {
    return (
      <div className="p-8 text-center">
        <div className="text-slate-400 mb-2">尚未选择家庭</div>
        <p className="text-sm text-slate-400">请先在账号中心选择或创建家庭</p>
      </div>
    )
  }

  const cards = [
    { label: '家庭成员', value: stats?.people_count ?? '-', icon: Users, color: 'text-blue-600 bg-blue-50', href: '/people' },
    { label: '即将到来的日程', value: stats?.upcoming_events ?? '-', icon: Calendar, color: 'text-purple-600 bg-purple-50', href: '/events' },
    { label: '待处理事务', value: stats?.pending_tasks ?? '-', icon: CheckSquare, color: 'text-orange-600 bg-orange-50', href: '/tasks' },
    { label: '物品资产', value: stats?.things_count ?? '-', icon: Package, color: 'text-pink-600 bg-pink-50', href: '/things' },
    { label: '生活空间', value: stats?.spaces_count ?? '-', icon: Home, color: 'text-green-600 bg-green-50', href: '/spaces' },
  ]

  return (
    <div className="p-6">
      <div className="mb-6">
        <h1 className="text-2xl font-bold text-slate-900">家庭概览</h1>
        <p className="text-slate-500 mt-1 text-sm">掌握家庭的一切动态</p>
      </div>

      <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-5 gap-4 mb-8">
        {cards.map((card, i) => (
          <motion.a
            key={card.label}
            href={card.href}
            initial={{ opacity: 0, y: 12 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ delay: i * 0.05 }}
            className="card hover:shadow-md transition-shadow cursor-pointer no-underline"
          >
            <div className={`w-10 h-10 rounded-xl ${card.color} flex items-center justify-center mb-3`}>
              <card.icon className="w-5 h-5" />
            </div>
            <div className="text-2xl font-bold text-slate-900">{card.value}</div>
            <div className="text-xs text-slate-500 mt-0.5">{card.label}</div>
          </motion.a>
        ))}
      </div>

      <div className="card">
        <div className="flex items-center space-x-2 mb-4">
          <TrendingUp className="w-4 h-4 text-primary-600" />
          <h2 className="font-semibold text-slate-900">快速导航</h2>
        </div>
        <div className="grid grid-cols-2 md:grid-cols-3 gap-3">
          {[
            { label: '添加家庭成员', href: '/people', desc: '记录成员信息' },
            { label: '创建日程事件', href: '/events', desc: '安排家庭活动' },
            { label: '添加待办事项', href: '/tasks', desc: '跟踪家庭任务' },
            { label: '登记家庭物品', href: '/things', desc: '管理家庭资产' },
            { label: '定义生活空间', href: '/spaces', desc: '规划家庭环境' },
          ].map((item) => (
            <a
              key={item.href}
              href={item.href}
              className="p-3 rounded-lg border border-slate-100 hover:border-primary-200 hover:bg-primary-50 transition-colors no-underline"
            >
              <div className="text-sm font-medium text-slate-900">{item.label}</div>
              <div className="text-xs text-slate-400 mt-0.5">{item.desc}</div>
            </a>
          ))}
        </div>
      </div>
    </div>
  )
}
