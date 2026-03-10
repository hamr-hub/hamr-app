import { Outlet, Link, useLocation } from 'react-router-dom'
import { LayoutDashboard, Users, Calendar, CheckSquare, Package, Home, LogOut } from 'lucide-react'

const navItems = [
  { path: '/dashboard', label: '概览', icon: LayoutDashboard },
  { path: '/people', label: '人员', icon: Users },
  { path: '/events', label: '日历', icon: Calendar },
  { path: '/tasks', label: '事务', icon: CheckSquare },
  { path: '/things', label: '物品', icon: Package },
  { path: '/spaces', label: '空间', icon: Home },
]

export default function Layout() {
  const location = useLocation()

  return (
    <div className="flex min-h-screen">
      <aside className="w-56 bg-white border-r border-slate-100 flex flex-col shrink-0">
        <div className="p-4 border-b border-slate-100">
          <div className="flex items-center space-x-2">
            <div className="w-8 h-8 bg-primary-600 rounded-lg flex items-center justify-center">
              <Home className="w-4 h-4 text-white" />
            </div>
            <div>
              <div className="font-bold text-sm text-slate-900">HamR 管家</div>
              <div className="text-xs text-slate-400">家庭智能助理</div>
            </div>
          </div>
        </div>

        <nav className="flex-1 p-3 space-y-0.5">
          {navItems.map(({ path, label, icon: Icon }) => {
            const active = location.pathname === path
            return (
              <Link
                key={path}
                to={path}
                className={`flex items-center space-x-2.5 px-3 py-2 rounded-lg text-sm font-medium transition-colors ${
                  active
                    ? 'bg-primary-50 text-primary-700'
                    : 'text-slate-600 hover:bg-slate-50 hover:text-slate-900'
                }`}
              >
                <Icon className="w-4 h-4 shrink-0" />
                <span>{label}</span>
              </Link>
            )
          })}
        </nav>

        <div className="p-3 border-t border-slate-100">
          <button
            onClick={() => {
              localStorage.clear()
              window.location.href = '/login'
            }}
            className="flex items-center space-x-2 w-full px-3 py-2 text-sm text-slate-500 hover:text-slate-900 rounded-lg hover:bg-slate-50 transition-colors"
          >
            <LogOut className="w-4 h-4" />
            <span>退出登录</span>
          </button>
        </div>
      </aside>

      <main className="flex-1 overflow-auto bg-slate-50">
        <Outlet />
      </main>
    </div>
  )
}
