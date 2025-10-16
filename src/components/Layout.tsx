import React from "react";
import { Link, useLocation } from "react-router-dom";
import logoSvg from "../assets/logo.svg";
import { CursorService } from "../services/cursorService";

interface LayoutProps {
  children: React.ReactNode;
}

export const Layout: React.FC<LayoutProps> = ({ children }) => {
  const location = useLocation();

  const handleOpenLogDirectory = async () => {
    try {
      await CursorService.openLogDirectory();
    } catch (error) {
      console.error("打开日志目录失败:", error);
    }
  };

  const navItems = [
    { path: "/", label: "首页", icon: "🏠" },
    { path: "/machine-id", label: "Machine ID 管理", icon: "🔧" },
    { path: "/auth-check", label: "授权检查", icon: "🔐" },
    { path: "/token-manage", label: "Token 管理", icon: "🎫" },
  ];

  return (
    <div className="flex flex-col min-h-screen bg-gray-50">
      {/* Navigation */}
      <nav className="bg-white border-b shadow-sm">
        <div className="px-4 mx-auto max-w-7xl sm:px-6 lg:px-8">
          <div className="flex justify-between h-16">
            <div className="flex">
              <div className="flex items-center flex-shrink-0">
                <Link to="/" className="flex items-center space-x-3">
                  <img
                    src={logoSvg}
                    alt="Cursor Manager Logo"
                    className="w-8 h-8"
                  />
                  <h1 className="text-xl font-bold text-gray-900">
                    Cursor Manager
                  </h1>
                </Link>
              </div>
              <div className="hidden sm:ml-6 sm:flex sm:space-x-8">
                {navItems.map((item) => (
                  <Link
                    key={item.path}
                    to={item.path}
                    className={`inline-flex items-center px-1 pt-1 border-b-2 text-sm font-medium ${
                      location.pathname === item.path
                        ? "border-blue-500 text-gray-900"
                        : "border-transparent text-gray-500 hover:border-gray-300 hover:text-gray-700"
                    }`}
                  >
                    <span className="mr-2">{item.icon}</span>
                    {item.label}
                  </Link>
                ))}
              </div>
            </div>
          </div>
        </div>
      </nav>

      {/* Main Content */}
      <main className="flex-1 py-6 mx-auto max-w-7xl sm:px-6 lg:px-8 min-w-[85vw]">
        <div className="px-4 py-6 sm:px-0">{children}</div>
      </main>

      {/* Footer with Disclaimer */}
      <footer className="mt-auto bg-white border-t border-gray-200">
        <div className="px-4 py-6 mx-auto max-w-7xl sm:px-6 lg:px-8">
          <div className="space-y-4 text-center">
            <div className="p-4 border border-yellow-200 rounded-lg bg-yellow-50">
              <h3 className="mb-2 text-sm font-semibold text-yellow-800">
                ⚠️ 免责声明
              </h3>
              <p className="text-xs leading-relaxed text-yellow-700">
                本工具仅供学习和研究目的使用。使用本工具产生的任何后果由用户自行承担，开发者不承担任何法律责任。
                请遵守相关服务条款和法律法规。如有任何问题或疑虑，请及时停止使用并联系开发者。
              </p>
            </div>
            <div className="text-xs text-gray-500">
              <p>
                如有问题请联系：
                <a
                  href="mailto:wuqi_y@163.com"
                  className="ml-1 text-blue-600 hover:text-blue-800"
                >
                  wuqi_y@163.com
                </a>
                <span className="mx-2">|</span>
                <button
                  onClick={handleOpenLogDirectory}
                  className="text-blue-600 hover:text-blue-800 hover:underline"
                >
                  📂 打开日志目录
                </button>
              </p>
              <p className="mt-1">
                © 2025 Cursor Manager. 仅供学习研究使用。
              </p>
            </div>
          </div>
        </div>
      </footer>
    </div>
  );
};
