# Cursor 机器ID 恢复工具

一个基于 Tauri 2 构建的桌面应用程序，用于管理和恢复 Cursor 编辑器的机器标识符。

## 功能特性

- 🔍 自动检测 Cursor 编辑器安装状态
- 💾 扫描和列出可用的机器ID备份文件
- 👁️ 预览备份中的机器ID信息
- 🔄 一键恢复机器ID设置
- 🛡️ 自动创建当前配置的备份
- 📊 查看账户使用情况和统计信息
- 🔐 安全的系统级ID更新
- 🎯 跨平台支持 (Windows, macOS, Linux)

## 功能说明

### 机器ID管理
- **备份管理**: 自动扫描和显示所有可用的机器ID备份
- **信息预览**: 查看备份文件中的完整机器ID信息，包括：
  - `telemetry.devDeviceId`
  - `telemetry.macMachineId`
  - `telemetry.machineId`
  - `telemetry.sqmId`
  - `storage.serviceMachineId`
- **一键恢复**: 安全地恢复选定的机器ID配置
- **自动备份**: 在恢复前自动创建当前配置的备份

### 账户信息查看
- **认证状态检查**: 验证当前账户的认证状态
- **使用情况统计**: 查看账户的使用情况和额度信息
- **订阅信息**: 显示账户的订阅状态和试用期信息

### Token管理
- **Token信息查看**: 显示当前的认证令牌信息
- **令牌验证**: 验证令牌的有效性和格式

## 系统要求

- Node.js 18+
- Rust 1.70+
- pnpm 或 npm
- Cursor 编辑器已安装并运行过至少一次

## 安装和运行

### 开发环境

1. 克隆仓库并安装依赖：
```bash
cd auto-cursor
pnpm install
```

2. 启动开发服务器：
```bash
pnpm tauri dev
```

### 构建应用

```bash
pnpm tauri build
```

## 使用说明

### 1. 启动应用
运行应用后，会自动检测系统中是否已安装 Cursor 编辑器。

### 2. 机器ID管理
- 在"机器ID"页面中，应用会自动扫描所有可用的备份文件
- 选择一个备份文件查看其中的机器ID信息
- 点击"恢复"按钮来应用选定的机器ID配置
- 应用会在恢复前自动创建当前配置的备份

### 3. 账户检查
- 在"认证检查"页面中，可以查看当前账户的认证状态
- 显示账户的使用情况、订阅信息和令牌状态

### 4. Token管理
- 在"Token管理"页面中，可以查看和管理认证令牌
- 验证令牌格式和有效性

## 技术架构

### 前端 (React + TypeScript)
- 现代化的 React UI 界面
- TypeScript 类型安全
- Tailwind CSS 样式框架
- 响应式设计

### 后端 (Rust + Tauri)
- Rust 高性能后端
- Tauri 2 桌面应用框架
- SQLite 数据库操作
- 跨平台文件系统访问
- 系统级权限操作

### Python脚本 (辅助功能)
- 认证信息管理
- 配置处理
- 令牌处理

## 文件路径

### Windows
- 存储文件: `%APPDATA%\Cursor\User\globalStorage\storage.json`
- 数据库: `%APPDATA%\Cursor\User\globalStorage\state.vscdb`
- 机器ID: `%APPDATA%\Cursor\machineId`

### macOS
- 存储文件: `~/Library/Application Support/Cursor/User/globalStorage/storage.json`
- 数据库: `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`
- 机器ID: `~/Library/Application Support/Cursor/machineId`

### Linux
- 存储文件: `~/.config/Cursor/User/globalStorage/storage.json`
- 数据库: `~/.config/Cursor/User/globalStorage/state.vscdb`
- 机器ID: `~/.config/Cursor/machineId`

## 安全说明

- 应用只读取和修改 Cursor 相关的配置文件
- 系统级操作需要相应权限
- 所有操作前都会创建备份
- 不会收集或上传任何用户数据
- 只与 Cursor 官方 API 进行必要通信

## 常见问题

### Q: 为什么需要管理员权限？
A: 某些系统级ID更新（如Windows注册表、macOS系统配置）需要提升权限。

### Q: 恢复失败怎么办？
A: 应用会显示详细的错误信息，并且已创建的备份可以用于手动恢复。

### Q: 支持哪些备份文件格式？
A: 支持标准的 JSON 格式备份文件，文件名格式为 `storage.json.bak.YYYYMMDD_HHMMSS`。

## 开发

### 项目结构
```
auto-cursor/
├── src/                    # React 前端源码
├── src-tauri/             # Rust 后端源码
│   ├── src/
│   │   ├── lib.rs         # 主入口
│   │   └── machine_id.rs  # 机器ID处理模块
│   ├── Cargo.toml         # Rust 依赖
│   └── tauri.conf.json    # Tauri 配置
├── package.json           # Node.js 依赖
└── tailwind.config.js     # Tailwind 配置
```

### 添加新功能
1. 后端：在 `src-tauri/src/` 中添加新的 Rust 模块
2. 前端：在 `src/` 中添加新的 React 组件
3. 配置：更新 `tauri.conf.json` 中的权限设置

## 许可证

MIT License
