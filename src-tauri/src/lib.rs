mod account_manager;
mod auth_checker;
mod logger;
mod machine_id;

use account_manager::{AccountListResult, AccountManager, LogoutResult, SwitchAccountResult};
use auth_checker::{AuthCheckResult, AuthChecker, TokenInfo};
use base64::{Engine as _, engine::general_purpose};
use chrono;
use machine_id::{BackupInfo, MachineIdRestorer, MachineIds, ResetResult, RestoreResult};
use rand::{Rng, distributions::Alphanumeric};
use regex::Regex;
// use reqwest;
use wreq::Client;
use wreq_util::Emulation;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tauri::{Emitter, Manager};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

// 日志宏现在在logger.rs中定义

// 获取应用目录的辅助函数
pub fn get_app_dir() -> Result<PathBuf, String> {
    let exe_path = env::current_exe().map_err(|e| format!("Failed to get exe path: {}", e))?;
    let app_dir = exe_path
        .parent()
        .ok_or("Failed to get parent directory")?
        .to_path_buf();
    Ok(app_dir)
}

// 创建隐藏窗口的Command（Windows平台适配）
fn create_hidden_command(executable_path: &str) -> Command {
    #[cfg(target_os = "windows")]
    {
        let mut cmd = Command::new(executable_path);
        // Windows平台：隐藏命令行窗口
        // CREATE_NO_WINDOW = 0x08000000
        cmd.creation_flags(0x08000000);
        cmd
    }

    #[cfg(not(target_os = "windows"))]
    {
        Command::new(executable_path)
    }
}

// 递归复制目录的辅助函数
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().unwrap();
        let dst_path = dst.join(name);
        if path.is_dir() {
            copy_dir_all(&path, &dst_path)?;
        } else {
            fs::copy(&path, &dst_path)?;
        }
    }
    Ok(())
}

// 复制 pyBuild 文件夹到应用目录
pub fn copy_pybuild_to_app_dir(app_handle: &tauri::AppHandle) -> Result<(), String> {
    let app_dir = get_app_dir()?;
    let src_dir = app_dir.join("pyBuild");

    // 创建目标目录
    fs::create_dir_all(&src_dir).map_err(|e| format!("Failed to create directory: {}", e))?;

    // 复制资源文件到工作目录
    let resource_dir = app_handle.path().resource_dir().unwrap().join("pyBuild");
    if resource_dir.exists() {
        log_info!("Found resource directory at: {:?}", resource_dir);

        // 如果目标目录已存在，先删除它以实现覆盖
        if src_dir.exists() {
            fs::remove_dir_all(&src_dir)
                .map_err(|e| format!("Failed to remove existing directory: {}", e))?;
        }

        // 递归复制目录
        if let Err(e) = copy_dir_all(&resource_dir, &src_dir) {
            log_error!("Failed to copy resource directory: {}", e);
            return Err(format!("Failed to copy pyBuild directory: {}", e));
        }

        log_info!("Successfully copied pyBuild to: {:?}", src_dir);
        Ok(())
    } else {
        log_info!("Resource directory not found at: {:?}", resource_dir);
        Err("Resource directory not found".to_string())
    }
}

// 获取Python可执行文件路径的辅助函数
fn get_python_executable_path() -> Result<PathBuf, String> {
    if cfg!(debug_assertions) {
        // 开发环境：使用相对于当前工作目录的路径
        let platform = if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        };

        let exe_name = if cfg!(target_os = "windows") {
            "cursor_register.exe"
        } else {
            "cursor_register"
        };

        Ok(get_app_dir()?.join("pyBuild").join(platform).join(exe_name))
    } else {
        // 生产环境：使用相对于exe的路径
        let current_exe =
            std::env::current_exe().map_err(|e| format!("无法获取当前执行文件路径: {}", e))?;
        let exe_dir = current_exe.parent().ok_or("无法获取执行文件目录")?;

        let platform = if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        };

        let exe_name = if cfg!(target_os = "windows") {
            "cursor_register.exe"
        } else {
            "cursor_register"
        };

        Ok(exe_dir.join("pyBuild").join(platform).join(exe_name))
    }
}

// 邮箱配置结构体
#[derive(Debug, Serialize, Deserialize, Clone)]
struct EmailConfig {
    worker_domain: String,
    email_domain: String,
    admin_password: String,
}

// Cloudflare临时邮箱相关结构体
#[derive(Debug, Serialize, Deserialize)]
struct CloudflareEmailResponse {
    jwt: Option<String>,
    address: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CloudflareMailsResponse {
    results: Option<Vec<CloudflareMail>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CloudflareMail {
    raw: Option<String>,
}

// 生成随机邮箱名称
fn generate_random_email_name() -> String {
    let letters1: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(5)
        .map(char::from)
        .collect::<String>()
        .to_lowercase();

    let numbers: String = (0..rand::thread_rng().gen_range(1..=3))
        .map(|_| rand::thread_rng().gen_range(0..10).to_string())
        .collect();

    let letters2: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(rand::thread_rng().gen_range(1..=3))
        .map(char::from)
        .collect::<String>()
        .to_lowercase();

    format!("{}{}{}", letters1, numbers, letters2)
}

// 创建临时邮箱
async fn create_cloudflare_temp_email() -> Result<(String, String), String> {
    let client = reqwest::Client::new();
    let random_name = generate_random_email_name();

    // 获取邮箱配置
    let email_config = get_email_config().await?;

    let url = format!("https://{}/admin/new_address", email_config.worker_domain);
    let payload = serde_json::json!({
        "enablePrefix": true,
        "name": random_name,
        "domain": email_config.email_domain,
    });

    log_debug!("创建邮箱请求详情:");
    log_debug!("  URL: {}", url);
    log_debug!("  Headers:");
    log_debug!("    x-admin-auth: {}", email_config.admin_password);
    log_debug!("    Content-Type: application/json");
    log_debug!(
        "  Payload: {}",
        serde_json::to_string_pretty(&payload).unwrap_or_default()
    );

    let response = client
        .post(&url)
        .header("X-Admin-Auth", &email_config.admin_password)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("创建邮箱请求失败: {}", e))?;

    let status = response.status();
    let headers = response.headers().clone();

    log_debug!("响应详情:");
    log_debug!("  状态码: {}", status);
    log_debug!("  响应头: {:?}", headers);

    // 获取响应文本用于调试
    let response_text = response
        .text()
        .await
        .map_err(|e| format!("读取响应文本失败: {}", e))?;

    log_info!("  响应体: {}", response_text);

    if status.is_success() {
        let data: CloudflareEmailResponse = serde_json::from_str(&response_text)
            .map_err(|e| format!("解析响应JSON失败: {} | 响应内容: {}", e, response_text))?;

        log_debug!("🔍 [DEBUG] 解析后的数据: {:?}", data);

        match (data.jwt, data.address) {
            (Some(jwt), Some(address)) => {
                log_info!("✅ 创建临时邮箱成功: {}", address);
                Ok((jwt, address))
            }
            _ => Err(format!(
                "响应中缺少JWT或邮箱地址 | 完整响应: {}",
                response_text
            )),
        }
    } else {
        Err(format!(
            "创建邮箱失败，状态码: {} | 响应内容: {}",
            status, response_text
        ))
    }
}

// 获取验证码
async fn get_verification_code_from_cloudflare(jwt: &str) -> Result<String, String> {
    let client = reqwest::Client::new();

    // 获取邮箱配置
    let email_config = get_email_config().await?;

    // 最多尝试30次，每次等待10秒
    for attempt in 1..=30 {
        log_debug!("🔍 第{}次尝试获取验证码...", attempt);

        let url = format!("https://{}/api/mails", email_config.worker_domain);
        log_debug!("🔍 [DEBUG] 获取邮件请求详情:");
        log_info!("  URL: {}", url);
        log_info!("  Headers:");
        log_info!("    Authorization: Bearer {}", jwt);
        log_info!("    Content-Type: application/json");
        log_info!("  Query: limit=10&offset=0");

        let response = client
            .get(&url)
            .header("Authorization", &format!("Bearer {}", jwt))
            .header("Content-Type", "application/json")
            .query(&[("limit", "10"), ("offset", "0")])
            .send()
            .await
            .map_err(|e| format!("获取邮件请求失败: {}", e))?;

        let status = response.status();
        log_debug!("🔍 [DEBUG] 获取邮件响应状态码: {}", status);

        if response.status().is_success() {
            let response_text = response
                .text()
                .await
                .map_err(|e| format!("读取邮件响应文本失败: {}", e))?;

            // log_debug!("🔍 [DEBUG] 邮件响应体: {}", response_text);

            let data: CloudflareMailsResponse =
                serde_json::from_str(&response_text).map_err(|e| {
                    format!("解析邮件响应JSON失败: {} | 响应内容: {}", e, response_text)
                })?;

            // log_debug!("🔍 [DEBUG] 解析后的邮件数据: {:?}", data);

            if let Some(results) = data.results {
                log_debug!("🔍 [DEBUG] 邮件数量: {}", results.len());
                if !results.is_empty() {
                    if let Some(raw_content) = &results[0].raw {
                        // log_debug!("🔍 [DEBUG] 第一封邮件原始内容: {}", raw_content);

                        // 使用正则表达式提取验证码 - 第一种方式
                        let re1 = Regex::new(r"code is (\d{6})").unwrap();
                        if let Some(captures) = re1.captures(raw_content) {
                            if let Some(code) = captures.get(1) {
                                let verification_code = code.as_str().to_string();
                                log_info!("✅ 成功提取验证码 (方式1): {}", verification_code);
                                return Ok(verification_code);
                            }
                        }

                        // 尝试第二种匹配方式
                        let re2 = Regex::new(r"code is:\s*(\d{6})").unwrap();
                        if let Some(captures) = re2.captures(raw_content) {
                            if let Some(code) = captures.get(1) {
                                let verification_code = code.as_str().to_string();
                                log_info!("✅ 成功提取验证码 (方式2): {}", verification_code);
                                return Ok(verification_code);
                            }
                        }
                        // 1. 移除颜色代码（如 #FF5733）
                        let color_code_regex = Regex::new(r"#([0-9a-fA-F]{6})\b").unwrap();
                        let content_without_colors = color_code_regex.replace_all(&raw_content, "");
                        
                        // 2. 移除前面是+号的6位数字（如 +123456）
                        let plus_regex = Regex::new(r"\+\d{6}").unwrap();
                        let content_without_plus = plus_regex.replace_all(&content_without_colors, "");
                        
                        // 3. 移除前面是@的6位数字（如 @123456）
                        let at_regex = Regex::new(r"@\d{6}").unwrap();
                        let content_without_at = at_regex.replace_all(&content_without_plus, "");
                        
                        // 4. 移除前面是=的6位数字（如 =123456）
                        let equal_regex = Regex::new(r"=\d{6}").unwrap();
                        let content_cleaned = equal_regex.replace_all(&content_without_at, "");

                        // 尝试第三种匹配方式：直接匹配连续的6位数字
                        let re3 = Regex::new(r"\b(\d{6})\b").unwrap();
                        if let Some(captures) = re3.captures(&content_cleaned) {
                            if let Some(code) = captures.get(1) {
                                let verification_code = code.as_str().to_string();
                                log_info!(
                                    "✅ 成功提取验证码 (方式3-连续6位数字): {}",
                                    verification_code
                                );
                                return Ok(verification_code);
                            }
                        }

                        log_debug!("🔍 [DEBUG] 未找到匹配的验证码模式");
                    } else {
                        log_debug!("🔍 [DEBUG] 第一封邮件没有raw内容");
                    }
                } else {
                    log_debug!("🔍 [DEBUG] 邮件列表为空");
                }
            } else {
                log_debug!("🔍 [DEBUG] 响应中没有results字段");
            }
        } else {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "无法读取错误响应".to_string());
            log_info!(
                "🔍 [DEBUG] 获取邮件失败，状态码: {} | 错误内容: {}",
                status,
                error_text
            );
        }

        // 等待10秒后重试
        log_info!("⏳ 等待10秒后重试...");
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    }

    Err("获取验证码超时".to_string())
}

// 从Outlook邮箱获取验证码
async fn get_verification_code_from_outlook(email: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let encoded_email = urlencoding::encode(email);

    // 最多尝试30次，每次等待10秒
    for attempt in 1..=30 {
        log_debug!("🔍 第{}次尝试从Outlook获取验证码...", attempt);

        // 获取收件箱邮件
        let inbox_url = format!(
            "http://query.paopaodw.com/api/GetLastEmails?email={}&boxType=1",
            encoded_email
        );
        log_debug!("🔍 [DEBUG] 获取收件箱邮件: {}", inbox_url);

        let inbox_response = client
            .get(&inbox_url)
            .send()
            .await
            .map_err(|e| format!("获取收件箱邮件失败: {}", e))?;

        if inbox_response.status().is_success() {
            let inbox_text = inbox_response
                .text()
                .await
                .map_err(|e| format!("读取收件箱响应失败: {}", e))?;

            log_debug!("🔍 [DEBUG] 收件箱响应: {}", inbox_text);

            if let Ok(inbox_data) = serde_json::from_str::<serde_json::Value>(&inbox_text) {
                if let Some(data) = inbox_data.get("data").and_then(|d| d.as_array()) {
                    for email_item in data {
                        if let Some(body) = email_item.get("Body").and_then(|b| b.as_str()) {
                            if let Some(code) = extract_verification_code_from_content(body) {
                                log_info!("✅ 从收件箱找到验证码: {}", code);
                                return Ok(code);
                            }
                        }
                    }
                }
            }
        }

        // 获取垃圾箱邮件
        let spam_url = format!(
            "http://query.paopaodw.com/api/GetLastEmails?email={}&boxType=2",
            encoded_email
        );
        log_debug!("🔍 [DEBUG] 获取垃圾箱邮件: {}", spam_url);

        let spam_response = client
            .get(&spam_url)
            .send()
            .await
            .map_err(|e| format!("获取垃圾箱邮件失败: {}", e))?;

        if spam_response.status().is_success() {
            let spam_text = spam_response
                .text()
                .await
                .map_err(|e| format!("读取垃圾箱响应失败: {}", e))?;

            log_debug!("🔍 [DEBUG] 垃圾箱响应: {}", spam_text);

            if let Ok(spam_data) = serde_json::from_str::<serde_json::Value>(&spam_text) {
                if let Some(data) = spam_data.get("data").and_then(|d| d.as_array()) {
                    for email_item in data {
                        if let Some(body) = email_item.get("Body").and_then(|b| b.as_str()) {
                            if let Some(code) = extract_verification_code_from_content(body) {
                                log_info!("✅ 从垃圾箱找到验证码: {}", code);
                                return Ok(code);
                            }
                        }
                    }
                }
            }
        }

        if attempt < 30 {
            log_info!("⏰ 第{}次尝试未找到验证码，等待10秒后重试...", attempt);
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        }
    }

    Err("获取验证码超时，请检查邮箱或稍后重试".to_string())
}

// 提取验证码的通用函数（复用现有逻辑）
fn extract_verification_code_from_content(content: &str) -> Option<String> {
    use regex::Regex;

    // 使用现有的验证码提取逻辑
    let re1 = Regex::new(r"code is (\d{6})").unwrap();
    if let Some(captures) = re1.captures(content) {
        if let Some(code) = captures.get(1) {
            return Some(code.as_str().to_string());
        }
    }

    // 第二种方式
    let re2 = Regex::new(r"验证码为：(\d{6})").unwrap();
    if let Some(captures) = re2.captures(content) {
        if let Some(code) = captures.get(1) {
            return Some(code.as_str().to_string());
        }
    }

    // 第三种方式
    let re3 = Regex::new(r"verification code is: (\d{6})").unwrap();
    if let Some(captures) = re3.captures(content) {
        if let Some(code) = captures.get(1) {
            return Some(code.as_str().to_string());
        }
    }

    // 第四种方式 - 更通用的6位数字匹配，排除颜色代码（如#414141）
    // 1. 移除颜色代码
    let color_code_regex = Regex::new(r"#([0-9a-fA-F]{6})\b").unwrap();
    // 移除前面是+号的6位数字
    let content_without_plus = content.replace(r"+\d{6}", "");
    let content_without_colors_plus = color_code_regex.replace_all(&content_without_plus, "");

    // 2. 查找 6 位数字
    let re4 = Regex::new(r"\b(\d{6})\b").unwrap();
    if let Some(captures) = re4.captures(&content_without_colors_plus) {
        if let Some(code) = captures.get(1) {
            return Some(code.as_str().to_string());
        }
    }

    None
}

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
async fn get_available_backups() -> Result<Vec<BackupInfo>, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    restorer
        .find_backups()
        .map_err(|e| format!("Failed to find backups: {}", e))
}

#[tauri::command]
async fn extract_backup_ids(backup_path: String) -> Result<MachineIds, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    restorer
        .extract_ids_from_backup(&backup_path)
        .map_err(|e| format!("Failed to extract IDs from backup: {}", e))
}

#[tauri::command]
async fn delete_backup(backup_path: String) -> Result<serde_json::Value, String> {
    use std::fs;

    match fs::remove_file(&backup_path) {
        Ok(_) => {
            log_info!("✅ 成功删除备份文件: {}", backup_path);
            Ok(serde_json::json!({
                "success": true,
                "message": "备份文件删除成功"
            }))
        }
        Err(e) => {
            log_error!("❌ 删除备份文件失败: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("删除失败: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn restore_machine_ids(backup_path: String) -> Result<RestoreResult, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    let mut details = Vec::new();
    let mut success = true;

    // Extract IDs from backup
    let ids = match restorer.extract_ids_from_backup(&backup_path) {
        Ok(ids) => {
            details.push("Successfully extracted IDs from backup".to_string());
            ids
        }
        Err(e) => {
            return Ok(RestoreResult {
                success: false,
                message: format!("Failed to extract IDs from backup: {}", e),
                details,
            });
        }
    };

    // Create backup of current state
    match restorer.create_backup() {
        Ok(backup_path) => {
            details.push(format!("Created backup at: {}", backup_path));
        }
        Err(e) => {
            details.push(format!("Warning: Failed to create backup: {}", e));
        }
    }

    // Update storage file
    if let Err(e) = restorer.update_storage_file(&ids) {
        success = false;
        details.push(format!("Failed to update storage file: {}", e));
    } else {
        details.push("Successfully updated storage.json".to_string());
    }

    // Update SQLite database (simplified version)
    match restorer.update_sqlite_db(&ids) {
        Ok(sqlite_results) => {
            details.extend(sqlite_results);
        }
        Err(e) => {
            details.push(format!("Warning: Failed to update SQLite database: {}", e));
        }
    }

    // Update machine ID file
    if let Err(e) = restorer.update_machine_id_file(&ids.dev_device_id) {
        details.push(format!("Warning: Failed to update machine ID file: {}", e));
    } else {
        details.push("Successfully updated machine ID file".to_string());
    }

    // Update system IDs
    match restorer.update_system_ids(&ids) {
        Ok(system_results) => {
            details.extend(system_results);
        }
        Err(e) => {
            details.push(format!("Warning: Failed to update system IDs: {}", e));
        }
    }

    let message = if success {
        "Machine IDs restored successfully".to_string()
    } else {
        "Machine ID restoration completed with some errors".to_string()
    };

    Ok(RestoreResult {
        success,
        message,
        details,
    })
}

#[tauri::command]
async fn get_cursor_paths() -> Result<(String, String), String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    Ok((
        restorer.db_path.to_string_lossy().to_string(),
        restorer.sqlite_path.to_string_lossy().to_string(),
    ))
}

#[tauri::command]
async fn check_cursor_installation() -> Result<bool, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    Ok(restorer.db_path.exists() || restorer.sqlite_path.exists())
}

#[tauri::command]
async fn reset_machine_ids() -> Result<ResetResult, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    restorer
        .reset_machine_ids()
        .map_err(|e| format!("Failed to reset machine IDs: {}", e))
}

#[tauri::command]
async fn complete_cursor_reset() -> Result<ResetResult, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    restorer
        .complete_cursor_reset()
        .map_err(|e| format!("Failed to complete Cursor reset: {}", e))
}

#[tauri::command]
async fn get_log_file_path() -> Result<String, String> {
    if let Some(log_path) = logger::Logger::get_log_path() {
        Ok(log_path.to_string_lossy().to_string())
    } else {
        Err("Logger not initialized".to_string())
    }
}

#[tauri::command]
async fn get_log_config() -> Result<serde_json::Value, String> {
    let (max_size_mb, log_file_name) = logger::get_log_config();
    Ok(serde_json::json!({
        "max_size_mb": max_size_mb,
        "log_file_name": log_file_name,
        "log_file_path": logger::Logger::get_log_path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "Not initialized".to_string())
    }))
}

#[tauri::command]
async fn test_logging() -> Result<String, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    restorer
        .test_logging()
        .map_err(|e| format!("Failed to test logging: {}", e))
}

#[tauri::command]
async fn debug_windows_cursor_paths() -> Result<Vec<String>, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    restorer
        .debug_windows_cursor_paths()
        .map_err(|e| format!("Failed to debug Windows cursor paths: {}", e))
}

#[tauri::command]
async fn set_custom_cursor_path(path: String) -> Result<String, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    restorer
        .set_custom_cursor_path(&path)
        .map_err(|e| format!("Failed to set custom cursor path: {}", e))
}

#[tauri::command]
async fn get_custom_cursor_path() -> Result<Option<String>, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    Ok(restorer.get_custom_cursor_path())
}

#[tauri::command]
async fn clear_custom_cursor_path() -> Result<String, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    restorer
        .clear_custom_cursor_path()
        .map_err(|e| format!("Failed to clear custom cursor path: {}", e))
}

#[tauri::command]
async fn open_log_file() -> Result<String, String> {
    // 使用新的日志系统获取日志文件路径
    let log_path = if let Some(path) = logger::Logger::get_log_path() {
        path
    } else {
        return Err("日志系统未初始化".to_string());
    };

    // 检查日志文件是否存在
    if !log_path.exists() {
        return Err("日志文件不存在，请先运行应用以生成日志".to_string());
    }

    let log_path_str = log_path.to_string_lossy().to_string();

    // 根据操作系统打开文件
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        Command::new("cmd")
            .args(["/C", "start", "", &log_path_str])
            .spawn()
            .map_err(|e| format!("Failed to open log file: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        Command::new("open")
            .arg(&log_path_str)
            .spawn()
            .map_err(|e| format!("Failed to open log file: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        Command::new("xdg-open")
            .arg(&log_path_str)
            .spawn()
            .map_err(|e| format!("Failed to open log file: {}", e))?;
    }

    Ok(format!("已打开日志文件: {}", log_path_str))
}

#[tauri::command]
async fn open_log_directory() -> Result<String, String> {
    // 使用新的日志系统获取日志文件路径
    let log_path = if let Some(path) = logger::Logger::get_log_path() {
        path
    } else {
        return Err("日志系统未初始化".to_string());
    };

    let log_dir = log_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let log_dir_str = log_dir.to_string_lossy().to_string();

    // 根据操作系统打开目录
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        Command::new("explorer")
            .arg(&log_dir_str)
            .spawn()
            .map_err(|e| format!("Failed to open log directory: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        Command::new("open")
            .arg(&log_dir_str)
            .spawn()
            .map_err(|e| format!("Failed to open log directory: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        Command::new("xdg-open")
            .arg(&log_dir_str)
            .spawn()
            .map_err(|e| format!("Failed to open log directory: {}", e))?;
    }

    Ok(format!("已打开日志目录: {}", log_dir_str))
}

#[tauri::command]
async fn get_current_machine_ids() -> Result<Option<MachineIds>, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    restorer
        .get_current_machine_ids()
        .map_err(|e| format!("Failed to get current machine IDs: {}", e))
}

#[tauri::command]
async fn get_machine_id_file_content() -> Result<Option<String>, String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    restorer
        .get_machine_id_file_content()
        .map_err(|e| format!("Failed to get machine ID file content: {}", e))
}

#[tauri::command]
async fn get_backup_directory_info() -> Result<(String, Vec<String>), String> {
    let restorer =
        MachineIdRestorer::new().map_err(|e| format!("Failed to initialize restorer: {}", e))?;

    restorer
        .get_backup_directory_info()
        .map_err(|e| format!("Failed to get backup directory info: {}", e))
}

#[tauri::command]
async fn check_user_authorization(token: String) -> Result<AuthCheckResult, String> {
    AuthChecker::check_user_authorized(&token)
        .await
        .map_err(|e| format!("Failed to check user authorization: {}", e))
}

#[tauri::command]
async fn get_user_info(token: String) -> Result<AuthCheckResult, String> {
    AuthChecker::get_user_info(&token)
        .await
        .map_err(|e| format!("Failed to get user info: {}", e))
}

#[tauri::command]
async fn get_token_auto() -> Result<TokenInfo, String> {
    Ok(AuthChecker::get_token_auto())
}

#[tauri::command]
async fn debug_cursor_paths() -> Result<Vec<String>, String> {
    AuthChecker::debug_cursor_paths().map_err(|e| format!("Failed to debug cursor paths: {}", e))
}

// Account Management Commands
#[tauri::command]
async fn get_account_list() -> Result<AccountListResult, String> {
    Ok(AccountManager::get_account_list())
}

#[tauri::command]
async fn add_account(
    email: String,
    token: String,
    refresh_token: Option<String>,
    workos_cursor_session_token: Option<String>,
) -> Result<serde_json::Value, String> {
    match AccountManager::add_account(
        email.clone(),
        token,
        refresh_token,
        workos_cursor_session_token,
    ) {
        Ok(()) => Ok(serde_json::json!({
            "success": true,
            "message": format!("Account {} added successfully", email)
        })),
        Err(e) => {
            let error_msg = e.to_string();
            // 如果是账号已存在的错误，返回 success: true
            if error_msg.contains("Account with this email already exists") {
                Ok(serde_json::json!({
                    "success": true,
                    "message": format!("Failed to add account: {}", error_msg)
                }))
            } else {
                Ok(serde_json::json!({
                    "success": false,
                    "message": format!("Failed to add account: {}", error_msg)
                }))
            }
        }
    }
}

#[tauri::command]
async fn switch_account(email: String) -> Result<SwitchAccountResult, String> {
    Ok(AccountManager::switch_account(email))
}

#[tauri::command]
async fn switch_account_with_token(
    email: String,
    token: String,
    auth_type: Option<String>,
) -> Result<SwitchAccountResult, String> {
    Ok(AccountManager::switch_account_with_token(
        email, token, auth_type,
    ))
}

#[tauri::command]
async fn edit_account(
    email: String,
    new_token: Option<String>,
    new_refresh_token: Option<String>,
    new_workos_cursor_session_token: Option<String>,
) -> Result<serde_json::Value, String> {
    log_info!(
        "🔍 [DEBUG] edit_account called with email: {}, new_token: {:?}, new_refresh_token: {:?}, new_workos_cursor_session_token: {:?}",
        email,
        new_token
            .as_ref()
            .map(|t| format!("{}...", &t[..t.len().min(10)])),
        new_refresh_token
            .as_ref()
            .map(|t| format!("{}...", &t[..t.len().min(10)])),
        new_workos_cursor_session_token
            .as_ref()
            .map(|t| format!("{}...", &t[..t.len().min(10)]))
    );

    match AccountManager::edit_account(
        email.clone(),
        new_token,
        new_refresh_token,
        new_workos_cursor_session_token,
    ) {
        Ok(()) => {
            log_info!("✅ [DEBUG] Account {} updated successfully", email);
            Ok(serde_json::json!({
                "success": true,
                "message": format!("Account {} updated successfully", email)
            }))
        }
        Err(e) => {
            log_error!("❌ [DEBUG] Failed to update account {}: {}", email, e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("Failed to update account: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn remove_account(email: String) -> Result<serde_json::Value, String> {
    match AccountManager::remove_account(email.clone()) {
        Ok(()) => Ok(serde_json::json!({
            "success": true,
            "message": format!("Account {} removed successfully", email)
        })),
        Err(e) => Ok(serde_json::json!({
            "success": false,
            "message": format!("Failed to remove account: {}", e)
        })),
    }
}

#[tauri::command]
async fn logout_current_account() -> Result<LogoutResult, String> {
    Ok(AccountManager::logout_current_account())
}

#[tauri::command]
async fn export_accounts(export_path: String) -> Result<serde_json::Value, String> {
    match AccountManager::export_accounts(export_path) {
        Ok(exported_path) => Ok(serde_json::json!({
            "success": true,
            "message": format!("账户导出成功: {}", exported_path),
            "exported_path": exported_path
        })),
        Err(e) => Ok(serde_json::json!({
            "success": false,
            "message": format!("导出失败: {}", e)
        })),
    }
}

#[tauri::command]
async fn import_accounts(import_file_path: String) -> Result<serde_json::Value, String> {
    match AccountManager::import_accounts(import_file_path) {
        Ok(message) => Ok(serde_json::json!({
            "success": true,
            "message": message
        })),
        Err(e) => Ok(serde_json::json!({
            "success": false,
            "message": format!("导入失败: {}", e)
        })),
    }
}

#[tauri::command]
async fn open_cancel_subscription_page(
    app: tauri::AppHandle,
    workos_cursor_session_token: String,
) -> Result<serde_json::Value, String> {
    log_info!("🔄 Opening cancel subscription page with WorkOS token...");

    let url = "https://cursor.com/dashboard?tab=billing";

    // 先尝试关闭已存在的窗口
    if let Some(existing_window) = app.get_webview_window("cancel_subscription") {
        log_info!("🔄 Closing existing cancel subscription window...");
        if let Err(e) = existing_window.close() {
            log_error!("❌ Failed to close existing window: {}", e);
        } else {
            log_info!("✅ Existing window closed successfully");
        }
        // 等待一小段时间确保窗口完全关闭
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // 创建新的 WebView 窗口（默认隐藏）
    let app_handle = app.clone();
    let webview_window = tauri::WebviewWindowBuilder::new(
        &app,
        "cancel_subscription",
        tauri::WebviewUrl::External(url.parse().unwrap()),
    )
    .title("Cursor - 取消订阅")
    .inner_size(1200.0, 800.0)
    .resizable(true)
    .initialization_script(&format!(
        r#"
        // 在页面加载前设置 Cookie
        document.cookie = 'WorkosCursorSessionToken={}; domain=.cursor.com; path=/; secure; samesite=none';
        console.log('Cookie injected via initialization script');
        
        // 可选：检查 Cookie 是否设置成功
        console.log('Current cookies:', document.cookie);
        "#,
        workos_cursor_session_token
    ))
    .on_page_load(move |_window, _payload| {
        // 在页面加载完成时注入 Cookie
        let cus_script = r#"
            function findAndClickCancelButton () {
            console.log('Current page URL:', window.location.href);

            const manBtn = document.querySelector('.dashboard-outline-button') || document.querySelector('.dashboard-outline-button-medium')
            if (manBtn) {
                console.log('找到了');
                manBtn.click();
                setTimeout(() => {
                manBtn.click();
                setTimeout(() => {
                    manBtn.click();
                }, 1000)
                }, 1000)
                setTimeout(() => {
                window.__TAURI_INTERNALS__.invoke('show_cancel_subscription_window');
                }, 1500)
            } else {
                if (location.href.includes('dashboard')) {
                window.__TAURI_INTERNALS__.invoke('cancel_subscription_failed');
                console.log('没找到按钮');
                }
            }
            }
            if (document.readyState === 'complete') {
            console.log('页面已经加载完成');
            setTimeout(() => {
                findAndClickCancelButton()
            }, 2500)
            } else {
            // 监听页面加载完成事件
            window.addEventListener('load', function () {
                console.log('window load 事件触发');
                setTimeout(() => {
                findAndClickCancelButton()
                }, 2500)
            });
            }
            "#;
        
        if let Err(e) = _window.eval(cus_script) {
            log_error!("❌ Failed to inject page load: {}", e);
        } else {
            log_info!("✅ Page load injected successfully on page load");
        }
    })
    .visible(true) // 默认隐藏窗口
    .build();

    match webview_window {
        Ok(window) => {
            // 添加窗口关闭事件监听器
            let app_handle_clone = app_handle.clone();
            window.on_window_event(move |event| {
                match event {
                    tauri::WindowEvent::CloseRequested { .. } => {
                        log_info!("🔄 Cancel subscription window close requested by user");
                        // 用户手动关闭窗口时，调用失败处理
                        let app_handle_clone = app_handle_clone.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = cancel_subscription_failed(app_handle_clone).await {
                                log_error!("❌ Failed to handle window close: {}", e);
                            }
                        });
                    }
                    tauri::WindowEvent::Destroyed => {
                        log_info!("🔄 Cancel subscription window destroyed");
                    }
                    _ => {}
                }
            });
            
            log_info!("✅ Successfully opened WebView window");
            Ok(serde_json::json!({
                "success": true,
                "message": "已打开取消订阅页面，正在自动登录..."
            }))
        }
        Err(e) => {
            log_error!("❌ Failed to create WebView window: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("无法打开内置浏览器: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn show_cancel_subscription_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("cancel_subscription") {
        // 延迟1500ms再显示窗口
        tokio::time::sleep(tokio::time::Duration::from_millis(2500)).await;

        window
            .show()
            .map_err(|e| format!("Failed to show window: {}", e))?;
        log_info!("✅ Cancel subscription window shown");

        // 发送事件通知前端操作成功
        if let Err(e) = app.emit("cancel-subscription-success", ()) {
            log_error!("❌ Failed to emit success event: {}", e);
        }
    }
    Ok(())
}

#[tauri::command]
async fn cancel_subscription_failed(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("cancel_subscription") {
        window
            .close()
            .map_err(|e| format!("Failed to close window: {}", e))?;
        log_error!("❌ Cancel subscription failed, window closed");

        // 发送事件通知前端操作失败
        if let Err(e) = app.emit("cancel-subscription-failed", ()) {
            log_error!("❌ Failed to emit failed event: {}", e);
        }
    }
    Ok(())
}

// 纯函数：获取绑卡链接
async fn get_bind_card_url_internal(
    workos_cursor_session_token: String,
) -> Result<String, String> {
    use wreq::header::{HeaderMap, HeaderValue, COOKIE};

    log_info!("🔄 Fetching bind card URL from Cursor API...");

    // 构建请求头
    let mut headers = HeaderMap::new();
    headers.insert(
        COOKIE,
        HeaderValue::from_str(&format!(
            "WorkosCursorSessionToken={}",
            workos_cursor_session_token
        ))
        .map_err(|e| format!("Failed to create cookie header: {}", e))?,
    );
    headers.insert("Content-Type", HeaderValue::from_static("application/json"));

    // 构建请求体
    let body = serde_json::json!({
        "tier": "pro"
    });

    // 创建 HTTP 客户端
    // let client = reqwest::Client::builder()
    //     .timeout(std::time::Duration::from_secs(30))
    //     .build()
    //     .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let client = Client::builder()
        .emulation(Emulation::Chrome141) // 这里是关键！选择模拟 Chrome 141
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    println!("Sending request with impersonated Chrome 141 TLS fingerprint...");

    // 将 JSON body 转换为字符串
    let body_string = serde_json::to_string(&body)
        .map_err(|e| format!("Failed to serialize body: {}", e))?;

    // 发送 POST 请求
    log_info!("📤 Sending POST request to https://cursor.com/api/checkout");
    let response = client
        .post("https://cursor.com/api/checkout")
        .headers(headers)
        .body(body_string)
        .send()
        .await
        .map_err(|e| format!("Failed to send request: {}", e))?;

    // 检查响应状态
    let status = response.status();
    log_info!("📥 Received response with status: {}", status);

    if !status.is_success() {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        log_error!("❌ API request failed: {} - {}", status, error_text);
        return Err(format!("API request failed: {} - {}", status, error_text));
    }

    // 获取响应文本（直接就是URL，可能带引号）
    let mut url = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    // 去除可能存在的引号
    url = url.trim().trim_matches('"').to_string();

    log_info!("✅ Successfully got bind card URL: {}", url);

    // 检查是否返回的是 dashboard 页面（说明已经绑卡）
    if url.contains("cursor.com/dashboard") {
        log_error!("❌ 返回的是 dashboard 页面，该账户可能已经绑卡");
        return Err("该账户可能已经绑定过银行卡，无法再次绑卡。如需更换银行卡，请先取消订阅后再试。".to_string());
    }

    // 检查是否是 Stripe checkout URL
    if !url.contains("checkout.stripe.com") {
        log_error!("❌ 返回的不是有效的 Stripe checkout URL: {}", url);
        return Err(format!("返回的不是有效的绑卡链接: {}", url));
    }

    Ok(url)
}

#[tauri::command]
async fn get_bind_card_url(
    _app: tauri::AppHandle,
    workos_cursor_session_token: String,
) -> Result<serde_json::Value, String> {
    match get_bind_card_url_internal(workos_cursor_session_token).await {
        Ok(url) => {
            // 复制到剪贴板
            #[cfg(target_os = "macos")]
            {
                use std::process::Command;
                let _ = Command::new("pbcopy")
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .and_then(|mut child| {
                        use std::io::Write;
                        if let Some(stdin) = child.stdin.as_mut() {
                            stdin.write_all(url.as_bytes())?;
                        }
                        child.wait()
                    });
            }
            
            #[cfg(target_os = "windows")]
            {
                use std::process::Command;
                let _ = Command::new("cmd")
                    .args(&["/C", &format!("echo {} | clip", url)])
                    .output();
            }
            
            #[cfg(target_os = "linux")]
            {
                use std::process::Command;
                let _ = Command::new("xclip")
                    .args(&["-selection", "clipboard"])
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .and_then(|mut child| {
                        use std::io::Write;
                        if let Some(stdin) = child.stdin.as_mut() {
                            stdin.write_all(url.as_bytes())?;
                        }
                        child.wait()
                    });
            }
            
            Ok(serde_json::json!({
                "success": true,
                "url": url,
                "message": "绑卡链接已复制到剪贴板"
            }))
        }
        Err(e) => Ok(serde_json::json!({
            "success": false,
            "message": format!("获取绑卡链接失败: {}", e)
        })),
    }
}

#[tauri::command]
async fn open_manual_bind_card_page(
    app: tauri::AppHandle,
    workos_cursor_session_token: String,
) -> Result<serde_json::Value, String> {
    log_info!("🔄 Opening manual bind card page with WorkOS token...");

    // 获取绑卡链接
    let url = match get_bind_card_url_internal(workos_cursor_session_token).await {
        Ok(url) => url,
        Err(e) => {
            log_error!("❌ Failed to get bind card URL: {}", e);
            return Ok(serde_json::json!({
                "success": false,
                "message": format!("获取绑卡链接失败: {}", e)
            }));
        }
    };

    // 先尝试关闭已存在的窗口
    if let Some(existing_window) = app.get_webview_window("manual_bind_card") {
        log_info!("🔄 Closing existing manual bind card window...");
        if let Err(e) = existing_window.close() {
            log_error!("❌ Failed to close existing window: {}", e);
        } else {
            log_info!("✅ Existing window closed successfully");
        }
        // 等待一小段时间确保窗口完全关闭
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // 解析 URL
    let parsed_url = match url.parse() {
        Ok(u) => u,
        Err(e) => {
            log_error!("❌ Failed to parse URL: {}", e);
            return Ok(serde_json::json!({
                "success": false,
                "message": format!("无效的URL格式: {}", e)
            }));
        }
    };

    // 创建新的 WebView 窗口
    let webview_window = tauri::WebviewWindowBuilder::new(
        &app,
        "manual_bind_card",
        tauri::WebviewUrl::External(parsed_url),
    )
    .title("Cursor - 手动绑卡")
    .inner_size(1200.0, 800.0)
    .resizable(true)
    .visible(true)
    .build();

    match webview_window {
        Ok(_window) => {
            log_info!("✅ Successfully opened bind card window");
            Ok(serde_json::json!({
                "success": true,
                "message": "已打开手动绑卡页面"
            }))
        }
        Err(e) => {
            log_error!("❌ Failed to create WebView window: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("无法打开内置浏览器: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn show_manual_bind_card_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("manual_bind_card") {
        // 延迟1000ms再显示窗口
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

        window
            .show()
            .map_err(|e| format!("Failed to show window: {}", e))?;
        log_info!("✅ Manual bind card window shown");

        // 发送事件通知前端操作成功
        if let Err(e) = app.emit("manual-bind-card-success", ()) {
            log_error!("❌ Failed to emit success event: {}", e);
        }
    }
    Ok(())
}

#[tauri::command]
async fn manual_bind_card_failed(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("manual_bind_card") {
        window
            .close()
            .map_err(|e| format!("Failed to close window: {}", e))?;
        log_error!("❌ Manual bind card failed, window closed");

        // 发送事件通知前端操作失败
        if let Err(e) = app.emit("manual-bind-card-failed", ()) {
            log_error!("❌ Failed to emit failed event: {}", e);
        }
    }
    Ok(())
}

#[tauri::command]
async fn delete_cursor_account(
    workos_cursor_session_token: String,
) -> Result<serde_json::Value, String> {
    use reqwest::header::{HeaderMap, HeaderValue};

    log_info!("🔄 开始调用 Cursor 删除账户 API...");

    // 构建请求头
    let mut headers = HeaderMap::new();
    headers.insert("Accept", HeaderValue::from_static("*/*"));
    headers.insert(
        "Accept-Encoding",
        HeaderValue::from_static("gzip, deflate, br, zstd"),
    );
    headers.insert(
        "Accept-Language",
        HeaderValue::from_static("en,zh-CN;q=0.9,zh;q=0.8,eu;q=0.7"),
    );
    headers.insert("Content-Type", HeaderValue::from_static("application/json"));
    headers.insert("Content-Length", HeaderValue::from_static("2"));
    headers.insert("Origin", HeaderValue::from_static("https://cursor.com"));
    headers.insert(
        "Referer",
        HeaderValue::from_static("https://cursor.com/cn/dashboard?tab=settings"),
    );
    headers.insert(
        "Sec-CH-UA",
        HeaderValue::from_static(
            "\"Not;A=Brand\";v=\"99\", \"Google Chrome\";v=\"139\", \"Chromium\";v=\"139\"",
        ),
    );
    headers.insert("Sec-CH-UA-Arch", HeaderValue::from_static("\"x86\""));
    headers.insert("Sec-CH-UA-Bitness", HeaderValue::from_static("\"64\""));
    headers.insert("Sec-CH-UA-Mobile", HeaderValue::from_static("?0"));
    headers.insert("Sec-CH-UA-Platform", HeaderValue::from_static("\"macOS\""));
    headers.insert(
        "Sec-CH-UA-Platform-Version",
        HeaderValue::from_static("\"15.3.1\""),
    );
    headers.insert("Sec-Fetch-Dest", HeaderValue::from_static("empty"));
    headers.insert("Sec-Fetch-Mode", HeaderValue::from_static("cors"));
    headers.insert("Sec-Fetch-Site", HeaderValue::from_static("same-origin"));
    headers.insert("User-Agent", HeaderValue::from_static("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36"));

    // 使用传入的 WorkosCursorSessionToken
    let cookie_value = format!("WorkosCursorSessionToken={}", workos_cursor_session_token);
    log_info!(
        "🔍 [DEBUG] Using WorkosCursorSessionToken: {}...",
        &workos_cursor_session_token[..workos_cursor_session_token.len().min(50)]
    );
    headers.insert(
        "Cookie",
        HeaderValue::from_str(&cookie_value).map_err(|e| format!("Invalid cookie value: {}", e))?,
    );

    // 创建 HTTP 客户端
    let client = reqwest::Client::new();

    // 发送请求
    match client
        .post("https://cursor.com/api/dashboard/delete-account")
        .headers(headers)
        .body("{}")
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let headers_map: std::collections::HashMap<String, String> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();

            log_debug!("📥 API 响应状态: {}", status);
            log_debug!("📥 响应头: {:?}", headers_map);

            match response.text().await {
                Ok(body) => {
                    log_debug!("📥 响应体: {}", body);

                    Ok(serde_json::json!({
                        "success": status.is_success(),
                        "status": status.as_u16(),
                        "message": if status.is_success() {
                            format!("✅ 删除账户请求成功！状态码: {}, 响应: {}", status, body)
                        } else {
                            format!("❌ 删除账户失败！状态码: {}, 响应: {}", status, body)
                        },
                        "response_body": body,
                        "response_headers": headers_map
                    }))
                }
                Err(e) => {
                    log_error!("❌ 读取响应体失败: {}", e);
                    Ok(serde_json::json!({
                        "success": false,
                        "status": status.as_u16(),
                        "message": format!("❌ 读取响应失败: {}", e),
                        "response_headers": headers_map
                    }))
                }
            }
        }
        Err(e) => {
            log_error!("❌ 网络请求失败: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("❌ 网络请求失败: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn trigger_authorization_login(
    uuid: String,
    challenge: String,
    workos_cursor_session_token: String,
) -> Result<serde_json::Value, String> {
    use reqwest::header::{HeaderMap, HeaderValue};

    log_info!("🔄 开始调用 Cursor 授权登录 API...");
    log_debug!("🔍 [DEBUG] UUID: {}", uuid);
    log_debug!("🔍 [DEBUG] Challenge: {}", challenge);

    // 构建请求头
    let mut headers = HeaderMap::new();
    // headers.insert("Accept", HeaderValue::from_static("*/*"));
    // headers.insert(
    //     "Accept-Encoding",
    //     HeaderValue::from_static("gzip, deflate, br, zstd"),
    // );
    // headers.insert(
    //     "Accept-Language",
    //     HeaderValue::from_static("en,zh-CN;q=0.9,zh;q=0.8,eu;q=0.7"),
    // );
    // headers.insert("Content-Type", HeaderValue::from_static("application/json"));
    // headers.insert("Content-Length", HeaderValue::from_static("2"));
    // headers.insert("Origin", HeaderValue::from_static("https://cursor.com"));
    // headers.insert(
    //     "Referer",
    //     HeaderValue::from_str(&format!(
    //         "https://cursor.com/cn/loginDeepControl?challenge={}&uuid={}&mode=login",
    //         challenge, uuid
    //     ))
    //     .map_err(|e| format!("Invalid Referer header value: {}", e))?,
    // );
    // headers.insert(
    //     "Sec-CH-UA",
    //     HeaderValue::from_static(
    //         "\"Not;A=Brand\";v=\"99\", \"Google Chrome\";v=\"139\", \"Chromium\";v=\"139\"",
    //     ),
    // );
    // headers.insert("Sec-CH-UA-Arch", HeaderValue::from_static("\"x86\""));
    // headers.insert("Sec-CH-UA-Bitness", HeaderValue::from_static("\"64\""));
    // headers.insert("Sec-CH-UA-Mobile", HeaderValue::from_static("?0"));
    // headers.insert("Sec-CH-UA-Platform", HeaderValue::from_static("\"macOS\""));
    // headers.insert(
    //     "Sec-CH-UA-Platform-Version",
    //     HeaderValue::from_static("\"15.3.1\""),
    // );
    // headers.insert("Sec-Fetch-Dest", HeaderValue::from_static("empty"));
    // headers.insert("Sec-Fetch-Mode", HeaderValue::from_static("cors"));
    // headers.insert("Sec-Fetch-Site", HeaderValue::from_static("same-origin"));
    // headers.insert("User-Agent", HeaderValue::from_static("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36"));

    // 使用传入的 WorkosCursorSessionToken
    let cookie_value = format!("WorkosCursorSessionToken={}", workos_cursor_session_token);
    log_info!(
        "🔍 [DEBUG] Using WorkosCursorSessionToken: {}...",
        &workos_cursor_session_token[..workos_cursor_session_token.len().min(50)]
    );
    headers.insert(
        "Cookie",
        HeaderValue::from_str(&cookie_value).map_err(|e| format!("Invalid cookie value: {}", e))?,
    );

    // 创建 HTTP 客户端
    let client = reqwest::Client::new();

    let payload = serde_json::json!({
        "challenge": challenge,
        "uuid": uuid,
    });

    // 发送请求
    match client
        .post("https://cursor.com/api/auth/loginDeepCallbackControl")
        .headers(headers)
        .json(&payload)
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let headers_map: std::collections::HashMap<String, String> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();

            log_debug!("📥 API 响应状态: {}", status);
            log_debug!("📥 响应头: {:?}", headers_map);

            match response.text().await {
                Ok(body) => {
                    log_debug!("📥 响应体: {}", body);

                    Ok(serde_json::json!({
                        "success": status.is_success(),
                        "status": status.as_u16(),
                        "message": if status.is_success() {
                            format!("✅ 授权登录请求成功！状态码: {}, 响应: {}", status, body)
                        } else {
                            format!("❌ 授权登录失败！状态码: {}, 响应: {}", status, body)
                        },
                        "response_body": body,
                        "response_headers": headers_map
                    }))
                }
                Err(e) => {
                    log_error!("❌ 读取响应体失败: {}", e);
                    Ok(serde_json::json!({
                        "success": false,
                        "status": status.as_u16(),
                        "message": format!("❌ 读取授权登录响应失败: {}", e),
                        "response_headers": headers_map
                    }))
                }
            }
        }
        Err(e) => {
            log_error!("❌ 网络请求授权登录失败: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("❌ 网络请求授权登录失败: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn trigger_authorization_login_poll(
    uuid: String,
    verifier: String,
) -> Result<serde_json::Value, String> {
    use reqwest::header::{HeaderMap, HeaderValue};

    log_info!("🔄 开始调用 Cursor 授权登录 Poll API...");
    log_debug!("🔍 [DEBUG] UUID: {}", uuid);
    log_debug!("🔍 [DEBUG] verifier: {}", verifier);

    // 构建请求头
    let mut headers = HeaderMap::new();
    headers.insert("Accept", HeaderValue::from_static("*/*"));
    headers.insert(
        "Accept-Encoding",
        HeaderValue::from_static("gzip, deflate, br, zstd"),
    );
    headers.insert(
        "Accept-Language",
        HeaderValue::from_static("en,zh-CN;q=0.9,zh;q=0.8,eu;q=0.7"),
    );
    headers.insert("Content-Type", HeaderValue::from_static("application/json"));
    headers.insert("Content-Length", HeaderValue::from_static("2"));
    headers.insert("Origin", HeaderValue::from_static("https://cursor.com"));
    headers.insert(
        "Sec-CH-UA",
        HeaderValue::from_static(
            "\"Not;A=Brand\";v=\"99\", \"Google Chrome\";v=\"139\", \"Chromium\";v=\"139\"",
        ),
    );
    headers.insert("Sec-CH-UA-Arch", HeaderValue::from_static("\"x86\""));
    headers.insert("Sec-CH-UA-Bitness", HeaderValue::from_static("\"64\""));
    headers.insert("Sec-CH-UA-Mobile", HeaderValue::from_static("?0"));
    headers.insert("Sec-CH-UA-Platform", HeaderValue::from_static("\"macOS\""));
    headers.insert(
        "Sec-CH-UA-Platform-Version",
        HeaderValue::from_static("\"15.3.1\""),
    );
    headers.insert("Sec-Fetch-Dest", HeaderValue::from_static("empty"));
    headers.insert("Sec-Fetch-Mode", HeaderValue::from_static("cors"));
    headers.insert("Sec-Fetch-Site", HeaderValue::from_static("same-origin"));
    headers.insert("User-Agent", HeaderValue::from_static("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36"));

    // 创建 HTTP 客户端
    let client = reqwest::Client::new();

    // 发送请求
    match client
        .get(&format!(
            "https://api2.cursor.sh/auth/poll?uuid={}&verifier={}",
            uuid, verifier
        ))
        .headers(headers)
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let headers_map: std::collections::HashMap<String, String> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();

            log_debug!("📥 API 响应状态: {}", status);
            log_debug!("📥 响应头: {:?}", headers_map);

            match response.text().await {
                Ok(body) => {
                    log_debug!("📥 响应体: {}", body);

                    Ok(serde_json::json!({
                        "success": status.is_success(),
                        "status": status.as_u16(),
                        "message": if status.is_success() {
                            format!("✅ 授权登录Poll请求成功！状态码: {}, 响应: {}", status, body)
                        } else {
                            format!("❌ 授权登录Poll失败！状态码: {}, 响应: {}", status, body)
                        },
                        "response_body": body,
                        "response_headers": headers_map
                    }))
                }
                Err(e) => {
                    log_error!("❌ 读取响应体失败: {}", e);
                    Ok(serde_json::json!({
                        "success": false,
                        "status": status.as_u16(),
                        "message": format!("❌ 读取授权登录Poll响应失败: {}", e),
                        "response_headers": headers_map
                    }))
                }
            }
        }
        Err(e) => {
            log_error!("❌ 网络请求授权登录Poll失败: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("❌ 网络请求授权登录Poll失败: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn get_usage_for_period(
    token: String,
    start_date: u64,
    end_date: u64,
    team_id: i32,
) -> Result<serde_json::Value, String> {
    log_info!(
        "🔍 获取用量数据请求: token长度={}, start_date={}, end_date={}, team_id={}",
        token.len(),
        start_date,
        end_date,
        team_id
    );

    match AuthChecker::get_usage_for_period(&token, start_date, end_date, team_id).await {
        Ok(Some(usage_data)) => {
            log_info!("✅ 成功获取用量数据");
            Ok(serde_json::json!({
                "success": true,
                "message": "Successfully retrieved usage data",
                "data": usage_data
            }))
        }
        Ok(None) => {
            log_warn!("⚠️ 未找到用量数据");
            Ok(serde_json::json!({
                "success": false,
                "message": "No usage data found"
            }))
        }
        Err(e) => {
            log_error!("❌ 获取用量数据失败: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("Failed to get usage data: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn get_user_analytics(
    token: String,
    team_id: i32,
    user_id: i32,
    start_date: String,
    end_date: String,
) -> Result<serde_json::Value, String> {
    log_info!(
        "🔍 获取用户分析数据 - team_id: {}, user_id: {}, 时间范围: {} 到 {}",
        team_id,
        user_id,
        start_date,
        end_date
    );

    match AuthChecker::get_user_analytics(&token, team_id, user_id, &start_date, &end_date).await {
        Ok(Some(analytics_data)) => {
            log_info!("✅ 成功获取用户分析数据");
            Ok(serde_json::json!({
                "success": true,
                "message": "Successfully retrieved user analytics data",
                "data": analytics_data
            }))
        }
        Ok(None) => {
            log_warn!("⚠️ 未找到用户分析数据");
            Ok(serde_json::json!({
                "success": false,
                "message": "No user analytics data found"
            }))
        }
        Err(e) => {
            log_error!("❌ 获取用户分析数据失败: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("Failed to get user analytics data: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn get_usage_events(
    token: String,
    team_id: i32,
    start_date: String,
    end_date: String,
    page: i32,
    page_size: i32,
) -> Result<serde_json::Value, String> {
    log_info!(
        "🔍 获取使用事件数据 - team_id: {}, 时间范围: {} 到 {}, 页码: {}, 页大小: {}",
        team_id,
        start_date,
        end_date,
        page,
        page_size
    );

    match AuthChecker::get_usage_events(&token, team_id, &start_date, &end_date, page, page_size)
        .await
    {
        Ok(Some(events_data)) => {
            log_info!("✅ 成功获取使用事件数据");
            Ok(serde_json::json!({
                "success": true,
                "message": "Successfully retrieved usage events data",
                "data": events_data
            }))
        }
        Ok(None) => {
            log_warn!("⚠️ 未找到使用事件数据");
            Ok(serde_json::json!({
                "success": false,
                "message": "No usage events data found"
            }))
        }
        Err(e) => {
            log_error!("❌ 获取使用事件数据失败: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("Failed to get usage events data: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn register_cursor_account(
    first_name: String,
    last_name: String,
) -> Result<serde_json::Value, String> {
    log_info!("🔄 开始注册 Cursor 账户...");
    log_info!("👤 姓名: {} {}", first_name, last_name);

    // 获取可执行文件路径
    let executable_path = get_python_executable_path()?;

    if !executable_path.exists() {
        return Err(format!("找不到Python可执行文件: {:?}", executable_path));
    }

    log_info!("🐍 调用Python可执行文件: {:?}", executable_path);

    // 生成随机邮箱
    let random_email = format!(
        "{}{}{}@gmail.com",
        first_name.to_lowercase(),
        last_name.to_lowercase(),
        rand::random::<u32>() % 1000
    );

    // 获取应用目录
    let app_dir = get_app_dir()?;
    let app_dir_str = app_dir.to_string_lossy().to_string();

    // 使用 Base64 编码应用目录路径，避免特殊字符问题
    let app_dir_base64 = general_purpose::STANDARD.encode(&app_dir_str);

    // 执行Python可执行文件
    let output = create_hidden_command(&executable_path.to_string_lossy())
        .arg(&random_email)
        .arg(&first_name)
        .arg(&last_name)
        .arg("true") // 默认使用无痕模式
        .arg(&app_dir_base64) // 使用 Base64 编码的应用目录参数
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动Python脚本: {}", e))?
        .wait_with_output()
        .map_err(|e| format!("等待Python脚本执行失败: {}", e))?;

    // 处理输出
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log_error!("❌ Python脚本执行失败: {}", stderr);
        return Err(format!("注册失败: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    log_info!("📝 Python脚本输出: {}", stdout);

    // 解析JSON响应
    let result: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("解析注册结果失败: {}", e))?;

    if result["success"].as_bool().unwrap_or(false) {
        // 注册成功，保存账户信息
        if let Some(email) = result["email"].as_str() {
            match AccountManager::add_account(
                email.to_string(),
                "python_registered_token".to_string(), // 临时token
                None,
                None,
            ) {
                Ok(_) => log_info!("💾 账户信息已保存"),
                Err(e) => log_warn!("⚠️ 保存账户信息失败: {}", e),
            }
        }

        log_info!("✅ 注册成功!");
        Ok(result)
    } else {
        let error_msg = result["error"].as_str().unwrap_or("未知错误");
        log_error!("❌ 泣册失败: {}", error_msg);
        Err(error_msg.to_string())
    }
}

#[tauri::command]
async fn create_temp_email() -> Result<serde_json::Value, String> {
    log_info!("📧 测试Python可执行文件...");

    // 获取可执行文件路径
    let executable_path = get_python_executable_path()?;

    if !executable_path.exists() {
        return Err(format!("找不到Python可执行文件: {:?}", executable_path));
    }

    // 获取应用目录
    let app_dir = get_app_dir()?;
    let app_dir_str = app_dir.to_string_lossy().to_string();

    // 使用 Base64 编码应用目录路径，避免特殊字符问题
    let app_dir_base64 = general_purpose::STANDARD.encode(&app_dir_str);

    // 执行Python可执行文件测试（传递一个测试邮箱）
    let output = create_hidden_command(&executable_path.to_string_lossy())
        .arg("test@example.com")
        .arg("Test")
        .arg("User")
        .arg("true") // 默认使用无痕模式
        .arg(&app_dir_base64) // 使用 Base64 编码的应用目录参数
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动Python脚本: {}", e))?
        .wait_with_output()
        .map_err(|e| format!("等待Python脚本执行失败: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("创建邮箱失败: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("解析邮箱结果失败: {}", e))?;

    Ok(result)
}

/// 批量注册账户（串行执行，一个接一个注册，更稳定）
#[tauri::command]
async fn batch_register_with_email(
    app: tauri::AppHandle,
    emails: Vec<String>,
    first_names: Vec<String>,
    last_names: Vec<String>,
    email_type: Option<String>,
    _outlook_mode: Option<String>, // 保留用于未来扩展
    use_incognito: Option<bool>,
    enable_bank_card_binding: Option<bool>,
    skip_phone_verification: Option<bool>,
    btn_index: Option<u32>,
    selected_card_indices: Option<Vec<u32>>, // 新增：选中的银行卡索引列表
) -> Result<serde_json::Value, String> {
    let email_type_str = email_type.as_deref().unwrap_or("custom");
    log_info!("🔄 批量注册 {} 个 Cursor 账户（串行模式，邮箱类型：{}）...", emails.len(), email_type_str);
    
    if emails.len() != first_names.len() || emails.len() != last_names.len() {
        return Err("邮箱、姓名数量不一致".to_string());
    }

    // 读取银行卡配置
    let bank_card_config = read_bank_card_config().await?;
    let bank_card_data: serde_json::Value = serde_json::from_str(&bank_card_config)
        .map_err(|e| format!("解析银行卡配置失败: {}", e))?;
    
    let all_cards = if let Some(cards_array) = bank_card_data.get("cards").and_then(|v| v.as_array()) {
        cards_array.clone()
    } else {
        // 如果是旧格式（单张卡），转换为数组
        vec![bank_card_data]
    };

    // 如果提供了选中的银行卡索引，则只使用选中的卡片
    let cards = if let Some(indices) = &selected_card_indices {
        let mut selected_cards = Vec::new();
        for &index in indices.iter() {
            if (index as usize) < all_cards.len() {
                selected_cards.push(all_cards[index as usize].clone());
            } else {
                return Err(format!("银行卡索引 {} 超出范围（总共 {} 张卡）", index, all_cards.len()));
            }
        }
        selected_cards
    } else {
        // 如果没有提供索引，使用所有卡片（保持向后兼容）
        all_cards
    };

    if enable_bank_card_binding.unwrap_or(true) && cards.len() < emails.len() {
        return Err(format!(
            "选中的银行卡数量({})少于注册账户数量({})，请选择足够的银行卡",
            cards.len(),
            emails.len()
        ));
    }

    log_info!("📋 准备使用 {} 张银行卡进行批量注册", cards.len());

    // 备份原始配置，以便注册完成后恢复（防止异常情况导致配置丢失）
    let _backup_result = backup_bank_card_config().await;

    // 串行执行注册，一个接一个
    let mut results = Vec::new();
    let mut errors = Vec::new();
    
    for i in 0..emails.len() {
        let email = emails[i].clone();
        let first_name = first_names[i].clone();
        let last_name = last_names[i].clone();
        
        let email_display = if email.is_empty() { "自动生成" } else { &email };
        log_info!("🎯 [任务 {}/{}] 开始注册: {}", i + 1, emails.len(), email_display);
        
        // 判断是否需要设置银行卡配置
        let card_index_for_task = if enable_bank_card_binding.unwrap_or(true) && i < cards.len() {
            Some(i as u32)
        } else {
            None
        };
        
        // 根据邮箱类型调用不同的注册函数
        let result = match email_type_str {
            "cloudflare_temp" => {
                log_info!("📧 [任务 {}/{}] 使用 Cloudflare 临时邮箱注册", i + 1, emails.len());
                
                // 临时设置当前任务的银行卡配置
                if let Some(_) = card_index_for_task {
                    let card_config = cards[i].clone();
                    let temp_config = serde_json::json!(card_config);
                    let config_str = serde_json::to_string_pretty(&temp_config)
                        .unwrap_or_else(|_| "{}".to_string());
                    let _ = save_bank_card_config(config_str).await;
                }
                
                register_with_cloudflare_temp_email(
                    app.clone(),
                    first_name.clone(),
                    last_name.clone(),
                    use_incognito,
                    enable_bank_card_binding,
                    skip_phone_verification,
                    btn_index,
                    None, // 不传索引，因为已经临时设置了配置
                )
                .await
            }
            "outlook" => {
                log_info!("📧 [任务 {}/{}] 使用 Outlook 邮箱注册: {}", i + 1, emails.len(), email);
                
                // 临时设置当前任务的银行卡配置
                if let Some(_) = card_index_for_task {
                    let card_config = cards[i].clone();
                    let temp_config = serde_json::json!(card_config);
                    let config_str = serde_json::to_string_pretty(&temp_config)
                        .unwrap_or_else(|_| "{}".to_string());
                    let _ = save_bank_card_config(config_str).await;
                }
                
                register_with_outlook(
                    app.clone(),
                    email.clone(),
                    first_name.clone(),
                    last_name.clone(),
                    use_incognito,
                    enable_bank_card_binding,
                    skip_phone_verification,
                    btn_index,
                    None, // 不传索引，因为已经临时设置了配置
                )
                .await
            }
            _ => {
                // custom 或其他：使用指定邮箱
                log_info!("📧 [任务 {}/{}] 使用自定义邮箱注册: {}", i + 1, emails.len(), email);
                
                // 临时设置当前任务的银行卡配置
                if let Some(_) = card_index_for_task {
                    let card_config = cards[i].clone();
                    let temp_config = serde_json::json!(card_config);
                    let config_str = serde_json::to_string_pretty(&temp_config)
                        .unwrap_or_else(|_| "{}".to_string());
                    let _ = save_bank_card_config(config_str).await;
                }
                
                register_with_email(
                    app.clone(),
                    email.clone(),
                    first_name.clone(),
                    last_name.clone(),
                    use_incognito,
                    enable_bank_card_binding,
                    skip_phone_verification,
                    btn_index,
                    None, // 不传索引，因为已经临时设置了配置
                )
                .await
            }
        };
        
        // 获取实际使用的邮箱（从结果中提取）
        let actual_email = match &result {
            Ok(result_data) => {
                result_data
                    .get("accountInfo")
                    .and_then(|info| info.get("email"))
                    .and_then(|e| e.as_str())
                    .unwrap_or(&email)
                    .to_string()
            }
            Err(_) => email.clone(),
        };
        
        match result {
            Ok(result) => {
                log_info!("✅ [任务 {}/{}] 注册成功: {}", i + 1, emails.len(), actual_email);
                results.push(serde_json::json!({
                    "index": i,
                    "email": actual_email,
                    "success": true,
                    "result": result
                }));
            }
            Err(e) => {
                log_error!("❌ [任务 {}/{}] 注册失败: {} - {}", i + 1, emails.len(), actual_email, e);
                errors.push(serde_json::json!({
                    "index": i,
                    "email": actual_email,
                    "success": false,
                    "error": e
                }));
            }
        }
        
        // 添加短暂延迟，让系统有时间清理资源
        if i < emails.len() - 1 {
            log_info!("⏱️  等待 2 秒后开始下一个注册任务...");
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    }

    // 恢复原始银行卡配置
    // 恢复原始配置（从备份恢复）
    if let Err(e) = restore_bank_card_config().await {
        log_warn!("⚠️ 恢复原始银行卡配置失败: {}", e);
    } else {
        log_info!("✅ 已恢复原始银行卡配置");
    }

    log_info!(
        "🎉 批量注册完成: {} 成功, {} 失败",
        results.len(),
        errors.len()
    );

    Ok(serde_json::json!({
        "success": true,
        "total": emails.len(),
        "succeeded": results.len(),
        "failed": errors.len(),
        "results": results,
        "errors": errors
    }))
}

#[tauri::command]
async fn register_with_email(
    app: tauri::AppHandle,
    email: String,
    first_name: String,
    last_name: String,
    use_incognito: Option<bool>,
    enable_bank_card_binding: Option<bool>,
    skip_phone_verification: Option<bool>,
    btn_index: Option<u32>,
    selected_card_index: Option<u32>, // 新增：选中的银行卡索引
) -> Result<serde_json::Value, String> {
    log_info!("🔄 [DEBUG] register_with_email 函数被调用");
    log_info!("🔄 使用指定邮箱注册 Cursor 账户...");
    log_info!("📧 邮箱: {}", email);
    log_info!("👤 姓名: {} {}", first_name, last_name);
    log_info!("🔍 跳过手机号验证: {:?}", skip_phone_verification);

    // 如果启用了银行卡绑定，先备份并设置银行卡配置
    if enable_bank_card_binding.unwrap_or(true) {
        log_info!("💳 准备设置银行卡配置...");
        
        // 备份原始配置
        let _backup_result = backup_bank_card_config().await;
        
        // 读取银行卡配置
        let bank_card_config = read_bank_card_config().await?;
        let bank_card_data: serde_json::Value = serde_json::from_str(&bank_card_config)
            .map_err(|e| format!("解析银行卡配置失败: {}", e))?;
        
        // 获取所有卡片
        let all_cards = if let Some(cards_array) = bank_card_data.get("cards").and_then(|v| v.as_array()) {
            cards_array.clone()
        } else {
            // 旧格式：整个配置就是一张卡
            vec![bank_card_data.clone()]
        };
        
        if all_cards.is_empty() {
            return Err("银行卡配置为空，请先配置至少一张银行卡".to_string());
        }
        
        // 根据索引选择卡片，如果没有提供索引则使用第一张
        let card_index = selected_card_index.unwrap_or(0) as usize;
        if card_index >= all_cards.len() {
            return Err(format!("银行卡索引 {} 超出范围（总共 {} 张卡）", card_index, all_cards.len()));
        }
        
        let selected_card = &all_cards[card_index];
        log_info!("✅ 选择使用卡片 {} 进行注册", card_index + 1);
        
        // 将选中的卡片配置写入文件（旧格式，供 Python 脚本读取）
        let config_str = serde_json::to_string_pretty(selected_card)
            .unwrap_or_else(|_| "{}".to_string());
        
        if let Err(e) = save_bank_card_config(config_str).await {
            log_error!("❌ 设置银行卡配置失败: {}", e);
            return Err(format!("设置银行卡配置失败: {}", e));
        } else {
            log_info!("✅ 已设置选中的银行卡配置");
        }
    }

    // 获取可执行文件路径
    let executable_path = get_python_executable_path()?;

    if !executable_path.exists() {
        return Err(format!("找不到Python可执行文件: {:?}", executable_path));
    }

    // 执行Python可执行文件
    let incognito_flag = if use_incognito.unwrap_or(true) {
        "true"
    } else {
        "false"
    };

    let bank_card_flag = if enable_bank_card_binding.unwrap_or(true) {
        "true"
    } else {
        "false"
    };

    let skip_phone_flag = if skip_phone_verification.unwrap_or(false) {
        "1"
    } else {
        "0"
    };

    // 获取应用目录
    let app_dir = get_app_dir()?;
    let app_dir_str = app_dir.to_string_lossy().to_string();

    // 使用 Base64 编码应用目录路径，避免特殊字符问题
    let app_dir_base64 = general_purpose::STANDARD.encode(&app_dir_str);

    // 构建配置JSON
    let config_json = serde_json::json!({
        "btnIndex": btn_index.unwrap_or(1)
    });
    let config_json_str = serde_json::to_string(&config_json)
        .unwrap_or_else(|_| "{}".to_string());

    // 调试：显示将要传递的所有参数
    log_debug!("🔍 [DEBUG] register_with_email 准备传递的参数:");
    log_info!("  - 参数1 (email): {}", email);
    log_info!("  - 参数2 (first_name): {}", first_name);
    log_info!("  - 参数3 (last_name): {}", last_name);
    log_info!("  - 参数4 (incognito_flag): {}", incognito_flag);
    log_info!("  - 参数5 (app_dir_str): {}", app_dir_str);
    log_info!("  - 参数5 (app_dir_base64): {}", app_dir_base64);
    log_info!("  - 参数6 (bank_card_flag): {}", bank_card_flag);
    log_info!("  - 参数7 (skip_phone_flag): {}", skip_phone_flag);
    log_info!("  - 参数8 (config_json): {}", config_json_str);
    log_info!("  - 预期参数总数: 9 (包括脚本名)");

    let mut child = create_hidden_command(&executable_path.to_string_lossy())
        .arg(&email)
        .arg(&first_name)
        .arg(&last_name)
        .arg(incognito_flag)
        .arg(&app_dir_base64) // 使用 Base64 编码的应用目录参数
        .arg(bank_card_flag) // 银行卡绑定标志
        .arg(skip_phone_flag) // 跳过手机号验证标志
        .arg(&config_json_str) // 配置JSON字符串
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动Python脚本: {}", e))?;

    log_debug!("🔍 [DEBUG] 当前工作目录: {:?}", app_dir_str);

    // 实时读取输出
    use std::io::{BufRead, BufReader};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    let stdout = child.stdout.take().ok_or("无法获取stdout")?;
    let stderr = child.stderr.take().ok_or("无法获取stderr")?;

    let output_lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let error_lines = Arc::new(Mutex::new(Vec::<String>::new()));

    let output_lines_clone = output_lines.clone();
    let error_lines_clone = error_lines.clone();
    let app_clone = app.clone();

    // 启动线程读取stdout
    let stdout_thread = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(line) = line {
                log_info!("Python输出: {}", line);

                // 发送实时输出事件到前端
                if let Err(e) = app_clone.emit(
                    "registration-output",
                    serde_json::json!({
                        "type": "stdout",
                        "line": line.clone()
                    }),
                ) {
                    log_info!("发送事件失败: {}", e);
                } else {
                    let truncated = line.chars().take(50).collect::<String>();
                    log_info!("✅ 事件已发送: {}", truncated);
                }

                // 检查是否需要验证码
                if line.contains("等待前端输入验证码") || line.contains("request_verification_code")
                {
                    let _ = app_clone.emit(
                        "verification-code-required",
                        serde_json::json!({
                            "message": "请输入验证码"
                        }),
                    );
                }
                
                // 检查验证码是否超时，需要手动输入
                if line.contains("verification_timeout") || line.contains("manual_input_required") {
                    log_info!("⏰ 验证码获取超时，需要用户手动输入");
                    let _ = app_clone.emit(
                        "verification-code-timeout",
                        "自动获取验证码超时，请手动输入验证码",
                    );
                }

                if let Ok(mut lines) = output_lines_clone.lock() {
                    lines.push(line);
                }
            }
        }
    });

    // 启动线程读取stderr
    let app_clone2 = app.clone();
    let stderr_thread = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                log_info!("Python错误: {}", line);

                // 发送错误输出事件到前端
                let _ = app_clone2.emit(
                    "registration-output",
                    serde_json::json!({
                        "type": "stderr",
                        "line": line.clone()
                    }),
                );

                if let Ok(mut lines) = error_lines_clone.lock() {
                    lines.push(line);
                }
            }
        }
    });

    // 等待一段时间或者进程结束
    let start_time = Instant::now();
    let max_wait_time = Duration::from_secs(150); // 给足够时间输入验证码

    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                // 进程已结束
                break;
            }
            Ok(None) => {
                // 进程仍在运行
                if start_time.elapsed() > max_wait_time {
                    // 超时，终止进程
                    let _ = child.kill();
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(format!("检查进程状态失败: {}", e));
            }
        }
    }

    // 等待读取线程完成
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();

    // 获取最终输出
    let final_output_lines = output_lines.lock().unwrap().clone();
    let final_error_lines = error_lines.lock().unwrap().clone();

    log_info!("收集到 {} 行输出", final_output_lines.len());
    log_info!("收集到 {} 行错误", final_error_lines.len());

    // 构建输出字符串
    let stdout_str = final_output_lines.join("\n");
    let stderr_str = final_error_lines.join("\n");

    // 尝试解析最后一行的JSON输出
    let mut result: serde_json::Value = serde_json::json!({
        "success": false,
        "error": "未找到有效的JSON输出",
        "output_lines": final_output_lines,
        "raw_output": stdout_str
    });

    // 从后往前查找有效的JSON
    for line in final_output_lines.iter().rev() {
        if line.trim().starts_with('{') {
            match serde_json::from_str::<serde_json::Value>(line.trim()) {
                Ok(mut parsed) => {
                    // 将输出信息添加到结果中
                    parsed["output_lines"] = serde_json::json!(final_output_lines);
                    parsed["raw_output"] = serde_json::json!(stdout_str);
                    if !stderr_str.is_empty() {
                        parsed["error_output"] = serde_json::json!(stderr_str);
                    }
                    result = parsed;
                    break;
                }
                Err(_) => continue,
            }
        }
    }
    // 前端触发保存
    // if result["success"].as_bool().unwrap_or(false) {
    //     // 注册成功，保存账户信息
    //     let token = result["token"]
    //         .as_str()
    //         .unwrap_or("python_registered_token")
    //         .to_string();
    //     let workos_token = result["workos_cursor_session_token"]
    //         .as_str()
    //         .map(|s| s.to_string());

    //     log_info!("🔑 提取的token: {}", token);
    //     if let Some(ref workos) = workos_token {
    //         log_info!(
    //             "🔐 WorkosCursorSessionToken: {}...",
    //             &workos[..std::cmp::min(50, workos.len())]
    //         );
    //     }

    //     match AccountManager::add_account(
    //         email.clone(),
    //         token,
    //         None,         // refresh_token
    //         workos_token, // workos_cursor_session_token
    //     ) {
    //         Ok(_) => log_info!("💾 账户信息已保存"),
    //         Err(e) => log_warn!("⚠️ 保存账户信息失败: {}", e),
    //     }
    // }

    // 恢复原始配置（从备份恢复）
    if enable_bank_card_binding.unwrap_or(true) {
        let _ = restore_bank_card_config().await;
    }

    Ok(result)
}

#[tauri::command]
async fn register_with_cloudflare_temp_email(
    app: tauri::AppHandle,
    first_name: String,
    last_name: String,
    use_incognito: Option<bool>,
    enable_bank_card_binding: Option<bool>,
    skip_phone_verification: Option<bool>,
    btn_index: Option<u32>,
    selected_card_index: Option<u32>, // 新增：选中的银行卡索引
) -> Result<serde_json::Value, String> {
    log_info!("🔄 使用Cloudflare临时邮箱注册 Cursor 账户...");
    log_info!("👤 姓名: {} {}", first_name, last_name);
    log_info!(
        "🔍 [DEBUG] 前端传递的 use_incognito 参数: {:?}",
        use_incognito
    );
    log_info!("🔍 跳过手机号验证: {:?}", skip_phone_verification);

    // 如果启用了银行卡绑定，先备份并设置银行卡配置
    if enable_bank_card_binding.unwrap_or(true) {
        log_info!("💳 准备设置银行卡配置...");
        
        // 备份原始配置
        let _backup_result = backup_bank_card_config().await;
        
        // 读取银行卡配置
        let bank_card_config = read_bank_card_config().await?;
        let bank_card_data: serde_json::Value = serde_json::from_str(&bank_card_config)
            .map_err(|e| format!("解析银行卡配置失败: {}", e))?;
        
        // 获取所有卡片
        let all_cards = if let Some(cards_array) = bank_card_data.get("cards").and_then(|v| v.as_array()) {
            cards_array.clone()
        } else {
            // 旧格式：整个配置就是一张卡
            vec![bank_card_data.clone()]
        };
        
        if all_cards.is_empty() {
            return Err("银行卡配置为空，请先配置至少一张银行卡".to_string());
        }
        
        // 根据索引选择卡片，如果没有提供索引则使用第一张
        let card_index = selected_card_index.unwrap_or(0) as usize;
        if card_index >= all_cards.len() {
            return Err(format!("银行卡索引 {} 超出范围（总共 {} 张卡）", card_index, all_cards.len()));
        }
        
        let selected_card = &all_cards[card_index];
        log_info!("✅ 选择使用卡片 {} 进行注册", card_index + 1);
        
        // 将选中的卡片配置写入文件（旧格式，供 Python 脚本读取）
        let config_str = serde_json::to_string_pretty(selected_card)
            .unwrap_or_else(|_| "{}".to_string());
        
        if let Err(e) = save_bank_card_config(config_str).await {
            log_error!("❌ 设置银行卡配置失败: {}", e);
            return Err(format!("设置银行卡配置失败: {}", e));
        } else {
            log_info!("✅ 已设置银行卡配置为第一张卡");
        }
    }

    // 1. 创建临时邮箱
    let (jwt, email) = create_cloudflare_temp_email().await?;
    log_info!("📧 创建的临时邮箱: {}", email);

    // 2. 获取可执行文件路径
    let executable_path = get_python_executable_path()?;

    if !executable_path.exists() {
        return Err(format!("找不到Python可执行文件: {:?}", executable_path));
    }

    // 3. 启动注册进程并设置实时输出
    let incognito_flag = if use_incognito.unwrap_or(true) {
        "true"
    } else {
        "false"
    };

    let bank_card_flag = if enable_bank_card_binding.unwrap_or(true) {
        "true"
    } else {
        "false"
    };

    let skip_phone_flag = if skip_phone_verification.unwrap_or(false) {
        "1"
    } else {
        "0"
    };

    // 获取应用目录
    let app_dir = get_app_dir()?;
    let app_dir_str = app_dir.to_string_lossy().to_string();

    // 使用 Base64 编码应用目录路径，避免特殊字符问题
    let app_dir_base64 = general_purpose::STANDARD.encode(&app_dir_str);

    // 构建配置JSON
    let config_json = serde_json::json!({
        "btnIndex": btn_index.unwrap_or(1)
    });
    let config_json_str = serde_json::to_string(&config_json)
        .unwrap_or_else(|_| "{}".to_string());

    // 调试日志
    log_debug!("🔍 [DEBUG] Rust 启动Python脚本:");
    log_info!("  - 可执行文件: {:?}", executable_path);
    log_info!("  - 邮箱: {}", email);
    log_info!("  - 姓名: {} {}", first_name, last_name);
    log_info!("  - use_incognito 原始值: {:?}", use_incognito);
    log_info!("  - incognito_flag: {}", incognito_flag);
    log_info!("  - bank_card_flag: {}", bank_card_flag);
    log_info!("  - skip_phone_flag: {}", skip_phone_flag);
    log_info!("  - config_json: {}", config_json_str);
    log_info!("  - app_dir: {}", app_dir_str);
    log_info!("  - app_dir_base64: {}", app_dir_base64);
    log_info!(
        "  - 传递的参数: [{}, {}, {}, {}, {}, {}, {}, {}]",
        email,
        first_name,
        last_name,
        incognito_flag,
        app_dir_base64,
        bank_card_flag,
        skip_phone_flag,
        config_json_str
    );

    let mut child = create_hidden_command(&executable_path.to_string_lossy())
        .arg(&email)
        .arg(&first_name)
        .arg(&last_name)
        .arg(incognito_flag)
        .arg(&app_dir_base64) // 使用 Base64 编码的应用目录参数
        .arg(bank_card_flag) // 银行卡绑定标志
        .arg(skip_phone_flag) // 跳过手机号验证标志
        .arg(&config_json_str) // 配置JSON字符串
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动Python脚本: {}", e))?;

    // 获取stdout用于实时读取
    let stdout = child.stdout.take().ok_or("无法获取Python脚本的stdout")?;

    // 启动实时输出读取任务
    let app_for_output = app.clone();
    let jwt_for_verification = jwt.clone();
    let app_for_verification = app.clone();

    // 使用Arc<AtomicBool>来跟踪是否需要获取验证码
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let verification_needed = Arc::new(AtomicBool::new(false));
    let verification_needed_clone = verification_needed.clone();

    // 启动实时输出读取任务（在单独线程中）
    let app_clone = app_for_output.clone();
    let verification_needed_clone = verification_needed_clone.clone();
    let jwt_clone = jwt_for_verification.clone();
    let app_verification_clone = app_for_verification.clone();

    let output_task = std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};

        let reader = BufReader::new(stdout);

        for line in reader.lines() {
            match line {
                Ok(line_content) => {
                    log_info!("📝 Python输出: {}", line_content);

                    // 检查是否需要验证码
                    if line_content.contains("等待验证码")
                        || line_content.contains("request_verification_code")
                    {
                        log_debug!("🔍 检测到验证码请求，开始自动获取验证码...");
                        verification_needed_clone.store(true, Ordering::Relaxed);

                        // 启动验证码获取任务
                        let jwt_task = jwt_clone.clone();
                        let app_task = app_verification_clone.clone();
                        std::thread::spawn(move || {
                            // 使用tokio运行时
                            let rt = tokio::runtime::Runtime::new().unwrap();
                            rt.block_on(async {
                                // 等待一小段时间让邮件到达
                                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                                for attempt in 1..=10 {
                                    log_debug!("🔍 第{}次尝试获取验证码...", attempt);

                                    match get_verification_code_from_cloudflare(&jwt_task).await {
                                        Ok(code) => {
                                            log_info!("🎯 自动获取到验证码: {}", code);

                                            // 将验证码写入临时文件
                                            let temp_dir = std::env::temp_dir();
                                            let code_file =
                                                temp_dir.join("cursor_verification_code.txt");

                                            if let Err(e) = std::fs::write(&code_file, &code) {
                                                log_error!("❌ 写入验证码文件失败: {}", e);
                                                return;
                                            }

                                            // 发送事件通知前端
                                            if let Err(e) = app_task
                                                .emit("verification-code-auto-filled", &code)
                                            {
                                                log_error!("❌ 发送验证码事件失败: {}", e);
                                            }

                                            log_info!("✅ 验证码已自动填入临时文件");
                                            return;
                                        }
                                        Err(e) => {
                                            log_debug!("🔍 第{}次获取验证码失败: {}", attempt, e);
                                            if attempt < 10 {
                                                tokio::time::sleep(
                                                    tokio::time::Duration::from_secs(10),
                                                )
                                                .await;
                                            }
                                        }
                                    }
                                }

                                log_error!("❌ 自动获取验证码失败，已尝试10次");
                                if let Err(emit_err) =
                                    app_task.emit("verification-code-failed", "获取验证码失败")
                                {
                                    log_error!("❌ 发送失败事件失败: {}", emit_err);
                                }
                            });
                        });
                    }

                         // 检查验证码是否超时，需要手动输入
                    if line_content.contains("verification_timeout") || line_content.contains("manual_input_required") {
                        log_info!("⏰ 验证码获取超时，需要用户手动输入");
                        let _ = app_clone.emit(
                            "verification-code-timeout",
                            "自动获取验证码超时，请手动输入验证码",
                        );
                    }

                    // 发送实时输出到前端       
                    if let Err(e) = app_clone.emit(
                        "registration-output",
                        serde_json::json!({
                            "line": line_content,
                            "timestamp": chrono::Utc::now().to_rfc3339()
                        }),
                    ) {
                        log_error!("❌ 发送输出事件失败: {}", e);
                    }
                }
                Err(e) => {
                    log_error!("❌ 读取Python输出失败: {}", e);
                    break;
                }
            }
        }
    });

    // 验证码获取已集成到输出读取任务中

    // 4. 等待注册进程完成
    let exit_status = child
        .wait()
        .map_err(|e| format!("等待Python脚本执行失败: {}", e))?;

    log_debug!("🔍 Python进程已结束");

    // 等待输出读取任务完成
    let _ = output_task.join();

    // 6. 处理进程退出状态
    if !exit_status.success() {
        log_error!("❌ Python脚本执行失败，退出码: {:?}", exit_status.code());
        return Err(format!(
            "Python脚本执行失败，退出码: {:?}",
            exit_status.code()
        ));
    }

    // 7. 由于我们已经通过实时输出获取了所有信息，这里需要从最后的输出中解析结果
    // 我们可以通过检查临时文件或其他方式来获取最终结果
    // 简化处理：返回一个成功的结果，具体的注册状态通过实时输出已经传递给前端
    let result = serde_json::json!({
        // "success": true,
        // "message": "注册流程已完成",
        "email": email,
        "email_type": "cloudflare_temp"
    });

    // 8. 邮箱信息已经在创建result时添加了，这里不需要重复添加

    // 9. 如果注册成功，保存账户信息-前端保存
    // if result["success"].as_bool().unwrap_or(false) {
    //     let token = result["token"]
    //         .as_str()
    //         .unwrap_or("python_registered_token")
    //         .to_string();
    //     let workos_token = result["workos_cursor_session_token"]
    //         .as_str()
    //         .map(|s| s.to_string());

    //     log_info!("🔑 提取的token: {}", token);
    //     if let Some(ref workos) = workos_token {
    //         log_info!(
    //             "🔐 WorkosCursorSessionToken: {}...",
    //             &workos[..std::cmp::min(50, workos.len())]
    //         );
    //     }

    //     match AccountManager::add_account(
    //         email.clone(),
    //         token,
    //         None,         // refresh_token
    //         workos_token, // workos_cursor_session_token
    //     ) {
    //         Ok(_) => log_info!("💾 账户信息已保存"),
    //         Err(e) => log_warn!("⚠️ 保存账户信息失败: {}", e),
    //     }
    // }

    // 恢复原始配置（从备份恢复）
    if enable_bank_card_binding.unwrap_or(true) {
        let _ = restore_bank_card_config().await;
    }

    Ok(result)
}

// 使用Outlook邮箱注册账户
#[tauri::command]
async fn register_with_outlook(
    app: tauri::AppHandle,
    email: String,
    first_name: String,
    last_name: String,
    use_incognito: Option<bool>,
    enable_bank_card_binding: Option<bool>,
    skip_phone_verification: Option<bool>,
    btn_index: Option<u32>,
    selected_card_index: Option<u32>, // 新增：选中的银行卡索引
) -> Result<serde_json::Value, String> {
    log_info!("🔄 使用Outlook邮箱注册 Cursor 账户...");
    log_info!("📧 邮箱: {}", email);
    log_info!("👤 姓名: {} {}", first_name, last_name);
    log_info!("🔍 跳过手机号验证: {:?}", skip_phone_verification);
    log_info!(
        "🔍 [DEBUG] 前端传递的 use_incognito 参数: {:?}",
        use_incognito
    );

    // 如果启用了银行卡绑定，先备份并设置银行卡配置（使用第一张卡）
    if enable_bank_card_binding.unwrap_or(true) {
        log_info!("💳 准备设置银行卡配置...");
        
        // 备份原始配置
        let _backup_result = backup_bank_card_config().await;
        
        // 读取银行卡配置
        let bank_card_config = read_bank_card_config().await?;
        let bank_card_data: serde_json::Value = serde_json::from_str(&bank_card_config)
            .map_err(|e| format!("解析银行卡配置失败: {}", e))?;
        
        // 获取所有卡片
        let all_cards = if let Some(cards_array) = bank_card_data.get("cards").and_then(|v| v.as_array()) {
            cards_array.clone()
        } else {
            // 旧格式：整个配置就是一张卡
            vec![bank_card_data.clone()]
        };
        
        if all_cards.is_empty() {
            return Err("银行卡配置为空，请先配置至少一张银行卡".to_string());
        }
        
        // 根据索引选择卡片，如果没有提供索引则使用第一张
        let card_index = selected_card_index.unwrap_or(0) as usize;
        if card_index >= all_cards.len() {
            return Err(format!("银行卡索引 {} 超出范围（总共 {} 张卡）", card_index, all_cards.len()));
        }
        
        let selected_card = &all_cards[card_index];
        log_info!("✅ 选择使用卡片 {} 进行注册", card_index + 1);
        
        // 将选中的卡片配置写入文件（旧格式，供 Python 脚本读取）
        let config_str = serde_json::to_string_pretty(selected_card)
            .unwrap_or_else(|_| "{}".to_string());
        
        if let Err(e) = save_bank_card_config(config_str).await {
            log_error!("❌ 设置银行卡配置失败: {}", e);
            return Err(format!("设置银行卡配置失败: {}", e));
        } else {
            log_info!("✅ 已设置选中的银行卡配置");
        }
    }

    // 获取可执行文件路径
    let executable_path = get_python_executable_path()?;

    if !executable_path.exists() {
        return Err(format!("找不到Python可执行文件: {:?}", executable_path));
    }

    // 启动注册进程并设置实时输出
    let incognito_flag = if use_incognito.unwrap_or(true) {
        "true"
    } else {
        "false"
    };

    let bank_card_flag = if enable_bank_card_binding.unwrap_or(true) {
        "true"
    } else {
        "false"
    };

    let skip_phone_flag = if skip_phone_verification.unwrap_or(false) {
        "1"
    } else {
        "0"
    };

    // 获取应用目录
    let app_dir = get_app_dir()?;
    let app_dir_str = app_dir.to_string_lossy().to_string();
    let app_dir_base64 = general_purpose::STANDARD.encode(&app_dir_str);

    // 构建配置JSON
    let config_json = serde_json::json!({
        "btnIndex": btn_index.unwrap_or(1)
    });
    let config_json_str = serde_json::to_string(&config_json)
        .unwrap_or_else(|_| "{}".to_string());

    log_debug!("🔍 [DEBUG] 准备启动注册进程");
    log_info!("    可执行文件: {:?}", executable_path);
    log_info!("    邮箱: {}", email);
    log_info!("    姓名: {} {}", first_name, last_name);
    log_info!("    隐身模式: {}", incognito_flag);
    log_info!("    银行卡绑定: {}", bank_card_flag);
    log_info!("    跳过手机号验证: {}", skip_phone_flag);
    log_info!("    配置JSON: {}", config_json_str);

    let mut cmd = create_hidden_command(&executable_path.to_string_lossy());
    cmd.arg(&email)
        .arg(&first_name)
        .arg(&last_name)
        .arg(incognito_flag)
        .arg(&app_dir_base64)
        .arg(bank_card_flag)
        .arg(skip_phone_flag)
        .arg(&config_json_str)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    log_debug!("🔍 [DEBUG] 命令行: {:?}", cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动注册进程: {}", e))?;

    let stdout = child.stdout.take().ok_or("无法获取stdout".to_string())?;

    let stderr = child.stderr.take().ok_or("无法获取stderr".to_string())?;

    // 启动实时输出读取任务（使用同步线程，与Cloudflare注册函数保持一致）
    let app_clone = app.clone();
    let email_clone = email.clone();

    // 处理stdout
    let app_for_stdout = app_clone.clone();
    let email_for_stdout = email_clone.clone();
    let stdout_task = std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let reader = BufReader::new(stdout);

        for line in reader.lines() {
            match line {
                Ok(line_content) => {
                    log_info!("📝 Python输出: {}", line_content);

                    // 检查是否需要验证码
                    if line_content.contains("等待验证码")
                        || line_content.contains("request_verification_code")
                        || line_content.contains("需要邮箱验证码")
                        || line_content.contains("请输入验证码")
                    {
                        log_debug!("🔍 检测到验证码请求，开始从Outlook获取验证码...");

                        // 启动验证码获取任务
                        let app_task = app_for_stdout.clone();
                        let email_task = email_for_stdout.clone();
                        std::thread::spawn(move || {
                            // 使用tokio运行时
                            let rt = tokio::runtime::Runtime::new().unwrap();
                            rt.block_on(async {
                                // 等待一小段时间让邮件到达
                                tokio::time::sleep(tokio::time::Duration::from_secs(8)).await;

                                for attempt in 1..=10 {
                                    log_debug!("🔍 第{}次尝试获取Outlook验证码...", attempt);

                                    match get_verification_code_from_outlook(&email_task).await {
                                        Ok(code) => {
                                            log_info!("🎯 自动获取到验证码: {}", code);

                                            // 将验证码写入临时文件
                                            let temp_dir = std::env::temp_dir();
                                            let code_file =
                                                temp_dir.join("cursor_verification_code.txt");

                                            if let Err(e) = std::fs::write(&code_file, &code) {
                                                log_error!("❌ 写入验证码文件失败: {}", e);
                                                return;
                                            }

                                            // 发送验证码到前端
                                            if let Err(e) =
                                                app_task.emit("verification-code-received", &code)
                                            {
                                                log_error!("❌ 发送验证码事件失败: {}", e);
                                            }

                                            log_info!("✅ 验证码已自动填入临时文件");
                                            return;
                                        }
                                        Err(e) => {
                                            log_debug!("🔍 第{}次获取验证码失败: {}", attempt, e);
                                            if attempt < 10 {
                                                std::thread::sleep(std::time::Duration::from_secs(
                                                    10,
                                                ));
                                            }
                                        }
                                    }
                                }

                                log_error!("❌ 自动获取验证码失败，已尝试10次，请用户手动输入");
                                if let Err(emit_err) = app_task.emit(
                                    "verification-code-manual-input-required",
                                    "自动获取验证码失败，请手动输入验证码",
                                ) {
                                    log_error!("❌ 发送手动输入提示事件失败: {}", emit_err);
                                }
                            });
                        });
                    }

                    // 发送实时输出到前端
                    if let Err(e) = app_for_stdout.emit(
                        "registration-output",
                        serde_json::json!({
                            "line": line_content,
                            "timestamp": chrono::Utc::now().to_rfc3339()
                        }),
                    ) {
                        log_error!("❌ 发送输出事件失败: {}", e);
                    }
                }
                Err(e) => {
                    log_error!("❌ 读取Python输出失败: {}", e);
                    break;
                }
            }
        }
    });

    // 处理stderr
    let app_for_stderr = app.clone();
    let _stderr_task = std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let reader = BufReader::new(stderr);

        for line in reader.lines() {
            match line {
                Ok(line_content) => {
                    log_info!("📝 Python错误: {}", line_content);

                    // 发送错误输出到前端
                    if let Err(e) = app_for_stderr.emit(
                        "registration-output",
                        serde_json::json!({
                            "line": line_content,
                            "timestamp": chrono::Utc::now().to_rfc3339()
                        }),
                    ) {
                        log_error!("❌ 发送错误输出事件失败: {}", e);
                    }
                }
                Err(e) => {
                    log_error!("❌ 读取Python错误输出失败: {}", e);
                    break;
                }
            }
        }
    });

    // // 等待进程完成
    // let exit_status = child
    //     .wait()
    //     .map_err(|e| format!("等待注册进程完成失败: {}", e))?;

    // log_debug!("🔍 Python进程已结束");

    // // 等待输出读取任务完成
    // let _ = stdout_task.join();
    // let _ = stderr_task.join();

    // log_debug!("🔍 [DEBUG] 注册完成");
    // log_info!("    退出代码: {:?}", exit_status.code());

    // // 构建返回结果
    // let result = if exit_status.success() {
    //     serde_json::json!({
    //         "success": false,
    //         "message": "进程关闭"
    //     })
    // } else {
    //     serde_json::json!({
    //         "success": false,
    //         "message": "进程关闭",
    //         "exit_code": exit_status.code()
    //     })
    // };

    // 4. 等待注册进程完成
    let exit_status = child
        .wait()
        .map_err(|e| format!("等待Python脚本执行失败: {}", e))?;

    log_debug!("🔍 Python进程已结束");

    // 等待输出读取任务完成
    let _ = stdout_task.join();

    // 6. 处理进程退出状态
    if !exit_status.success() {
        log_error!("❌ Python脚本执行失败，退出码: {:?}", exit_status.code());
        return Err(format!(
            "Python脚本执行失败，退出码: {:?}",
            exit_status.code()
        ));
    }

    // 7. 由于我们已经通过实时输出获取了所有信息，这里需要从最后的输出中解析结果
    // 我们可以通过检查临时文件或其他方式来获取最终结果
    // 简化处理：返回一个成功的结果，具体的注册状态通过实时输出已经传递给前端
    let result = serde_json::json!({
        "success": false,
        "message": "注册进程已退出",
        "email": email,
        "email_type": "outlook-default"
    });

    // 恢复原始配置（从备份恢复）
    if enable_bank_card_binding.unwrap_or(true) {
        let _ = restore_bank_card_config().await;
    }

    Ok(result)
}

#[tauri::command]
async fn submit_verification_code(code: String) -> Result<serde_json::Value, String> {
    log_info!("🔢 接收到验证码: {}", code);

    // 验证验证码格式
    if !code.chars().all(|c| c.is_ascii_digit()) || code.len() != 6 {
        return Err("验证码必须是6位数字".to_string());
    }

    // 将验证码写入临时文件，供Python脚本读取
    let temp_dir = std::env::temp_dir();
    let code_file = temp_dir.join("cursor_verification_code.txt");

    log_info!("📁 临时目录: {:?}", temp_dir);
    log_info!("📄 验证码文件: {:?}", code_file);

    match std::fs::write(&code_file, &code) {
        Ok(_) => {
            log_info!("✅ 验证码已保存到临时文件: {:?}", code_file);
            Ok(serde_json::json!({
                "success": true,
                "message": "验证码已提交"
            }))
        }
        Err(e) => Err(format!("保存验证码失败: {}", e)),
    }
}

#[tauri::command]
async fn cancel_registration() -> Result<String, String> {
    use std::fs;

    // 创建取消文件
    let temp_dir = std::env::temp_dir();
    let cancel_file = temp_dir.join("cursor_registration_cancel.txt");

    log_info!("📁 临时目录: {:?}", temp_dir);
    log_info!("🚫 取消文件: {:?}", cancel_file);

    match fs::write(&cancel_file, "cancel") {
        Ok(_) => {
            log_info!("🚫 注册取消请求已发送到: {:?}", cancel_file);
            Ok("注册已取消".to_string())
        }
        Err(e) => Err(format!("发送取消请求失败: {}", e)),
    }
}

#[tauri::command]
async fn get_saved_accounts() -> Result<Vec<serde_json::Value>, String> {
    // 获取已保存的账户列表功能暂时不可用
    match AccountManager::load_accounts() {
        Ok(accounts) => {
            // 将AccountInfo转换为serde_json::Value
            let json_accounts: Vec<serde_json::Value> = accounts
                .into_iter()
                .map(|account| serde_json::to_value(account).unwrap_or(serde_json::Value::Null))
                .collect();
            Ok(json_accounts)
        }
        Err(e) => Err(format!("获取保存的账户失败: {}", e)),
    }
}

// Bank Card Configuration Commands
#[tauri::command]
async fn read_bank_card_config() -> Result<String, String> {
    use std::fs;

    // 获取应用目录
    let app_dir = get_app_dir()?;
    let config_path = app_dir.join("bank_card_config.json");

    if config_path.exists() {
        fs::read_to_string(&config_path)
            .map_err(|e| format!("Failed to read bank card config: {}", e))
    } else {
        // 如果文件不存在，返回空字符串，前端会使用默认配置
        Ok(String::new())
    }
}

#[tauri::command]
async fn save_bank_card_config(config: String) -> Result<(), String> {
    use std::fs;

    // 获取应用目录
    let app_dir = get_app_dir()?;
    let config_path = app_dir.join("bank_card_config.json");

    // 验证JSON格式
    serde_json::from_str::<serde_json::Value>(&config)
        .map_err(|e| format!("Invalid JSON format: {}", e))?;

    fs::write(&config_path, config)
        .map_err(|e| format!("Failed to save bank card config: {}", e))?;

    log_info!("✅ 银行卡配置已保存到: {:?}", config_path);
    Ok(())
}

// 备份银行卡配置
async fn backup_bank_card_config() -> Result<String, String> {
    use std::fs;

    let app_dir = get_app_dir()?;
    let config_path = app_dir.join("bank_card_config.json");
    let backup_path = app_dir.join("bank_card_config.backup.json");

    if config_path.exists() {
        let config_content = fs::read_to_string(&config_path)
            .map_err(|e| format!("读取银行卡配置失败: {}", e))?;
        
        fs::write(&backup_path, &config_content)
            .map_err(|e| format!("备份银行卡配置失败: {}", e))?;
        
        log_info!("✅ 银行卡配置已备份到: {:?}", backup_path);
        Ok(config_content)
    } else {
        Ok(String::new())
    }
}

// 恢复银行卡配置
async fn restore_bank_card_config() -> Result<(), String> {
    use std::fs;

    let app_dir = get_app_dir()?;
    let config_path = app_dir.join("bank_card_config.json");
    let backup_path = app_dir.join("bank_card_config.backup.json");

    if backup_path.exists() {
        let backup_content = fs::read_to_string(&backup_path)
            .map_err(|e| format!("读取备份配置失败: {}", e))?;
        
        if !backup_content.is_empty() {
            fs::write(&config_path, backup_content)
                .map_err(|e| format!("恢复银行卡配置失败: {}", e))?;
            
            log_info!("✅ 银行卡配置已从备份恢复");
        }
        
        // 删除备份文件
        let _ = fs::remove_file(&backup_path);
    }

    Ok(())
}

// Email Configuration Commands
#[tauri::command]
async fn read_email_config() -> Result<String, String> {
    use std::fs;

    // 获取应用目录
    let app_dir = get_app_dir()?;
    let config_path = app_dir.join("email_config.json");

    if config_path.exists() {
        fs::read_to_string(&config_path).map_err(|e| format!("Failed to read email config: {}", e))
    } else {
        // 如果文件不存在，返回空字符串，前端会使用默认配置
        Ok(String::new())
    }
}

#[tauri::command]
async fn save_email_config(config: String) -> Result<(), String> {
    use std::fs;

    // 获取应用目录
    let app_dir = get_app_dir()?;
    let config_path = app_dir.join("email_config.json");

    // 验证JSON格式
    serde_json::from_str::<serde_json::Value>(&config)
        .map_err(|e| format!("Invalid JSON format: {}", e))?;

    fs::write(&config_path, config).map_err(|e| format!("Failed to save email config: {}", e))?;

    log_info!("✅ 邮箱配置已保存到: {:?}", config_path);
    Ok(())
}

// 获取应用版本
#[tauri::command]
async fn get_app_version(app: tauri::AppHandle) -> Result<String, String> {
    let package_info = app.package_info();
    Ok(package_info.version.to_string())
}

// 打开更新链接
#[tauri::command]
async fn open_update_url(url: String) -> Result<(), String> {
    use std::process::Command;

    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", &url])
            .spawn()
            .map_err(|e| format!("Failed to open URL: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("Failed to open URL: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("Failed to open URL: {}", e))?;
    }

    Ok(())
}

// 手动触发复制 pyBuild 文件夹的命令

#[tauri::command]
async fn copy_pybuild_resources(app_handle: tauri::AppHandle) -> Result<String, String> {
    if cfg!(debug_assertions) {
        log_info!("Development mode: Manually copying pyBuild directory");
    }
    copy_pybuild_to_app_dir(&app_handle)?;
    let env_type = if cfg!(debug_assertions) {
        "development"
    } else {
        "production"
    };
    Ok(format!(
        "pyBuild directory copied successfully in {} mode",
        env_type
    ))
}

// 获取邮箱配置的辅助函数
async fn get_email_config() -> Result<EmailConfig, String> {
    match read_email_config().await {
        Ok(config_str) if !config_str.is_empty() => {
            match serde_json::from_str::<EmailConfig>(&config_str) {
                Ok(config) => {
                    // 验证配置是否完整
                    if config.worker_domain.is_empty()
                        || config.email_domain.is_empty()
                        || config.admin_password.is_empty()
                    {
                        return Err("邮箱配置不完整，请先在前端配置邮箱域名和密码".to_string());
                    }
                    Ok(config)
                }
                Err(e) => Err(format!("解析邮箱配置失败: {}", e)),
            }
        }
        _ => Err("未找到邮箱配置，请先在前端配置邮箱域名和密码".to_string()),
    }
}


#[tauri::command]
async fn auto_login_and_get_cookie(
    app: tauri::AppHandle,
    email: String,
    password: String,
    show_window: Option<bool>,
) -> Result<serde_json::Value, String> {
    log_info!("🚀 开始自动登录获取Cookie: {}", email);

    // 检查是否已经有同名窗口，如果有则关闭
    if let Some(existing_window) = app.get_webview_window("auto_login") {
        log_info!("🔄 关闭现有的自动登录窗口");
        if let Err(e) = existing_window.close() {
            log_error!("❌ Failed to close existing auto login window: {}", e);
        } else {
            log_info!("✅ Existing auto login window closed successfully");
        }
        // 等待一小段时间确保窗口完全关闭
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // 根据参数决定是否显示窗口
    let should_show_window = show_window.unwrap_or(false);
    log_info!("🖥️ 窗口显示设置: {}", if should_show_window { "显示" } else { "隐藏" });
    
    // 创建新的 WebView 窗口（根据配置显示/隐藏，启用无痕模式）
    let webview_window = tauri::WebviewWindowBuilder::new(
        &app,
        "auto_login",
        tauri::WebviewUrl::External("https://authenticator.cursor.sh/".parse().unwrap()),
    )
    .title("Cursor - 自动登录")
    .inner_size(1200.0, 800.0)
    .resizable(true)
    .visible(should_show_window) // 根据参数决定是否显示
    .incognito(true) // 启用无痕模式
    .on_page_load(move |window, _payload| {
        let email_clone = email.clone();
        let password_clone = password.clone();
        
        // 创建自动登录脚本
        let login_script = format!(
            r#"
            (function() {{
                console.log('自动登录脚本已注入');
                
                function performLogin() {{
                    console.log('开始执行登录流程');
                    console.log('Current page URL:', window.location.href);
                    console.log('Page title:', document.title);
                    
                    // 检查是否已经登录成功（在dashboard页面）
                    if (window.location.href.includes('/dashboard')) {{
                        console.log('检测到已经在dashboard页面，直接获取cookie');
                        window.__TAURI_INTERNALS__.invoke('check_login_cookies');
                        return;
                    }}
                    
                    // 等待页面完全加载
                    if (document.readyState !== 'complete') {{
                        console.log('页面未完全加载，等待中...');
                        return;
                    }}
                    
                    // 步骤1: 填写邮箱
                    setTimeout(() => {{
                        console.log('步骤1: 填写邮箱');
                        const emailInput = document.querySelector('.rt-reset .rt-TextFieldInput');
                        if (emailInput) {{
                            emailInput.value = '{}';
                            console.log('邮箱已填写:', emailInput.value);
                            
                            // 触发input事件以确保值被正确设置
                            emailInput.dispatchEvent(new Event('input', {{ bubbles: true }}));
                            emailInput.dispatchEvent(new Event('change', {{ bubbles: true }}));
                        }} else {{
                            console.error('未找到邮箱输入框');
                        }}
                    }}, 1000);
                    
                    // 步骤2: 点击第一个按钮（继续）
                    setTimeout(() => {{
                        console.log('步骤2: 点击继续按钮');
                        const firstButton = document.querySelector('.BrandedButton');
                        if (firstButton) {{
                            firstButton.click();
                            console.log('继续按钮已点击');
                        }} else {{
                            console.error('未找到继续按钮');
                        }}
                    }}, 2000);
                    
                    // 步骤3: 填写密码
                    setTimeout(() => {{
                        console.log('步骤3: 填写密码');
                        const passwordInput = document.querySelector('[name="password"]');
                        if (passwordInput) {{
                            passwordInput.value = '{}';
                            console.log('密码已填写');
                            
                            // 触发input事件以确保值被正确设置
                            passwordInput.dispatchEvent(new Event('input', {{ bubbles: true }}));
                            passwordInput.dispatchEvent(new Event('change', {{ bubbles: true }}));
                        }} else {{
                            console.error('未找到密码输入框');
                        }}
                    }}, 6000);
                    
                    // 步骤4: 点击登录按钮
                    setTimeout(() => {{
                        console.log('步骤4: 点击登录按钮');
                        const loginButton = document.querySelector('.BrandedButton');
                        if (loginButton) {{
                            loginButton.click();
                            console.log('登录按钮已点击');
                            
                            // 等待登录完成后检查cookie
                            setTimeout(() => {{
                                console.log('检查登录状态和cookie');
                                checkLoginSuccess();
                            }}, 3000);
                        }} else {{
                            console.error('未找到登录按钮');
                        }}
                    }}, 9000);
                }}
                
                function checkLoginSuccess() {{
                    console.log('检查登录是否成功');
                    console.log('当前URL:', window.location.href);
                    
                    // 检查是否登录成功（通过URL变化或页面元素判断）
                    if (window.location.href.includes('/dashboard')) {{
                        console.log('登录成功，通知Rust获取cookie');
                        
                        // 通知Rust后端登录成功，让Rust获取httpOnly cookie
                        // window.__TAURI_INTERNALS__.invoke('check_login_cookies');
                    }} else {{
                        console.log('登录可能未完成，继续检查...');
                        // 再次检查
                        setTimeout(() => {{
                            checkLoginSuccess();
                        }}, 2000);
                    }}
                }}
                
                // 监听URL变化（用于检测重定向）
                let lastUrl = location.href;
                new MutationObserver(() => {{
                    const url = location.href;
                    if (url !== lastUrl) {{
                        lastUrl = url;
                        console.log('检测到URL变化:', url);
                        // 如果重定向到dashboard，直接获取cookie
                        if (url.includes('dashboard') || url.includes('app')) {{
                            console.log('重定向到dashboard，获取cookie');
                            setTimeout(() => {{
                                // window.__TAURI_INTERNALS__.invoke('check_login_cookies');
                            }}, 1000);
                        }}
                    }}
                }}).observe(document, {{ subtree: true, childList: true }});

                // 检查页面加载状态
                if (document.readyState === 'complete') {{
                    console.log('页面已经加载完成，开始登录流程');
                    setTimeout(() => {{
                        performLogin();
                    }}, 1000);
                }} else {{
                    // 监听页面加载完成事件
                    window.addEventListener('load', function() {{
                        console.log('window load 事件触发，开始登录流程');
                        setTimeout(() => {{
                            performLogin();
                        }}, 1000);
                    }});
                }}
            }})();
            "#,
            email_clone, password_clone
        );

        if let Err(e) = window.eval(&login_script) {
            log_error!("❌ Failed to inject login script: {}", e);
        } else {
            log_info!("✅ Login script injected successfully");
        }
    })
    .build();

    match webview_window {
        Ok(_window) => {
            let message = if should_show_window {
                "自动登录窗口已打开，正在执行登录流程..."
            } else {
                "正在后台执行自动登录流程..."
            };
            log_info!("✅ Successfully created auto login WebView window ({})", if should_show_window { "visible" } else { "hidden" });
            
            Ok(serde_json::json!({
                "success": true,
                "message": message
            }))
        }
        Err(e) => {
            log_error!("❌ Failed to create auto login WebView window: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("无法打开自动登录窗口: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn verification_code_login(
    app: tauri::AppHandle,
    email: String,
    verification_code: String,
    show_window: Option<bool>,
) -> Result<serde_json::Value, String> {
    log_info!("🚀 开始验证码登录: {}", email);

    // 检查是否已经有同名窗口，如果有则关闭
    if let Some(existing_window) = app.get_webview_window("verification_code_login") {
        log_info!("🔄 关闭现有的验证码登录窗口");
        if let Err(e) = existing_window.close() {
            log_error!("❌ Failed to close existing verification code login window: {}", e);
        } else {
            log_info!("✅ Existing verification code login window closed successfully");
        }
        // 等待一小段时间确保窗口完全关闭
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // 根据参数决定是否显示窗口
    let should_show_window = show_window.unwrap_or(false);
    log_info!("🖥️ 窗口显示设置: {}", if should_show_window { "显示" } else { "隐藏" });
    
    // 创建新的 WebView 窗口（根据配置显示/隐藏，启用无痕模式）
    let webview_window = tauri::WebviewWindowBuilder::new(
        &app,
        "verification_code_login",
        tauri::WebviewUrl::External("https://authenticator.cursor.sh/".parse().unwrap()),
    )
    .title("Cursor - 验证码登录")
    .inner_size(1200.0, 800.0)
    .resizable(true)
    .visible(should_show_window) // 根据参数决定是否显示
    .incognito(true) // 启用无痕模式
    .on_page_load(move |window, _payload| {
        let email_clone = email.clone();
        let code_clone = verification_code.clone();
        
        // 创建验证码登录脚本（先用自动登录的脚本，你后面修改）
        let login_script = format!(
            r#"
            (function() {{
                console.log('验证码登录脚本已注入');
                
                function performLogin() {{
                    console.log('开始执行验证码登录流程');
                    console.log('Current page URL:', window.location.href);
                    console.log('Page title:', document.title);
                    
                    // 检查是否已经登录成功（在dashboard页面）
                    if (window.location.href.includes('/dashboard')) {{
                        console.log('检测到已经在dashboard页面，直接获取cookie');
                        window.__TAURI_INTERNALS__.invoke('check_verification_login_cookies');
                        return;
                    }}
                    
                    // 等待页面完全加载
                    if (document.readyState !== 'complete') {{
                        console.log('页面未完全加载，等待中...');
                        return;
                    }}
                    
                    // TODO: 你需要修改这里的脚本来实现验证码登录
                    // 步骤1: 填写邮箱
                    setTimeout(() => {{
                        console.log('步骤1: 填写邮箱');
                        const emailInput = document.querySelector('.rt-reset .rt-TextFieldInput');
                        if (emailInput) {{
                            emailInput.value = '{}';
                            console.log('邮箱已填写:', emailInput.value);
                            
                            // 触发input事件以确保值被正确设置
                            emailInput.dispatchEvent(new Event('input', {{ bubbles: true }}));
                            emailInput.dispatchEvent(new Event('change', {{ bubbles: true }}));
                        }} else {{
                            console.error('未找到邮箱输入框');
                        }}
                    }}, 1000);
                    
                    // 步骤2: 点击第一个按钮（继续）
                    setTimeout(() => {{
                        console.log('步骤2: 点击继续按钮');
                        const firstButton = document.querySelector('.BrandedButton');
                        if (firstButton) {{
                            firstButton.click();
                            console.log('继续按钮已点击');
                        }} else {{
                            console.error('未找到继续按钮');
                        }}
                    }}, 2000);
                            
                     // 点击验证码登录
                     setTimeout(() => {{
                        console.log('步骤2: 点击继续按钮');
                        const firstButton2 = document.querySelector('.rt-Button.ak-AuthButton');

                        if (firstButton2) {{
                            firstButton2.click();
                            console.log('继续按钮已点击');
                        }} else {{
                            console.error('未找到继续按钮');
                        }}
                    }}, 6000);
                    
                    // // 步骤3: 填写验证码（这里需要修改）
                    // setTimeout(() => {{
                    //     console.log('步骤3: 填写验证码');
                    //     // TODO: 修改为验证码输入框的选择器
                    //     const codeInput = document.querySelector('[name="verification_code"]');
                    //     if (codeInput) {{
                    //         codeInput.value = '{}';
                    //         console.log('验证码已填写');
                            
                    //         // 触发input事件以确保值被正确设置
                    //         codeInput.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    //         codeInput.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    //     }} else {{
                    //         console.error('未找到验证码输入框');
                    //     }}
                    // }}, 6000);
                    
                    // // 步骤4: 点击登录按钮
                    // setTimeout(() => {{
                    //     console.log('步骤4: 点击登录按钮');
                    //     const loginButton = document.querySelector('.BrandedButton');
                    //     if (loginButton) {{
                    //         loginButton.click();
                    //         console.log('登录按钮已点击');
                            
                    //         // 等待登录完成后检查cookie
                    //         setTimeout(() => {{
                    //             console.log('检查登录状态和cookie');
                    //             checkLoginSuccess();
                    //         }}, 3000);
                    //     }} else {{
                    //         console.error('未找到登录按钮');
                    //     }}
                    // }}, 9000);
                }}
                
                function checkLoginSuccess() {{
                    console.log('检查登录是否成功');
                    console.log('当前URL:', window.location.href);
                    
                    // 检查是否登录成功（通过URL变化或页面元素判断）
                    if (window.location.href.includes('/dashboard')) {{
                        console.log('登录成功，通知Rust获取cookie');
                        // 通知Rust后端登录成功，让Rust获取httpOnly cookie
                        // window.__TAURI_INTERNALS__.invoke('check_verification_login_cookies');
                    }} else {{
                        console.log('登录可能未完成，继续检查...');
                        // 再次检查
                        setTimeout(() => {{
                            checkLoginSuccess();
                        }}, 2000);
                    }}
                }}
                
                // 监听URL变化（用于检测重定向）
                let lastUrl = location.href;
                new MutationObserver(() => {{
                    const url = location.href;
                    if (url !== lastUrl) {{
                        lastUrl = url;
                        console.log('检测到URL变化:', url);
                        // 如果重定向到dashboard，直接获取cookie
                        if (url.includes('dashboard') || url.includes('app')) {{
                            console.log('重定向到dashboard，获取cookie');
                            setTimeout(() => {{
                                // window.__TAURI_INTERNALS__.invoke('check_verification_login_cookies');
                            }}, 1000);
                        }}
                    }}
                }}).observe(document, {{ subtree: true, childList: true }});

                // 检查页面加载状态
                if (document.readyState === 'complete') {{
                    console.log('页面已经加载完成，开始登录流程');
                    setTimeout(() => {{
                        performLogin();
                    }}, 1000);
                }} else {{
                    // 监听页面加载完成事件
                    window.addEventListener('load', function() {{
                        console.log('window load 事件触发，开始登录流程');
                        setTimeout(() => {{
                            performLogin();
                        }}, 1000);
                    }});
                }}
            }})();
            "#,
            email_clone, code_clone
        );

        if let Err(e) = window.eval(&login_script) {
            log_error!("❌ Failed to inject verification code login script: {}", e);
        } else {
            log_info!("✅ Verification code login script injected successfully");
        }
    })
    .build();

    match webview_window {
        Ok(_window) => {
            let message = if should_show_window {
                "验证码登录窗口已打开，正在执行登录流程..."
            } else {
                "正在后台执行验证码登录流程..."
            };
            log_info!("✅ Successfully created verification code login WebView window ({})", if should_show_window { "visible" } else { "hidden" });
            
            Ok(serde_json::json!({
                "success": true,
                "message": message
            }))
        }
        Err(e) => {
            log_error!("❌ Failed to create verification code login WebView window: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("无法打开验证码登录窗口: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn check_verification_login_cookies(app: tauri::AppHandle) -> Result<(), String> {
    log_info!("🔍 开始检查验证码登录Cookie");
    
    if let Some(window) = app.get_webview_window("verification_code_login") {
        // 尝试多个可能的URL来获取cookie
        let urls_to_try = vec![
            "https://authenticator.cursor.sh/",
            "https://cursor.com/",
            "https://app.cursor.com/",
            "https://www.cursor.com/",
        ];
        
        for url_str in urls_to_try {
            log_info!("🔍 尝试从 {} 获取cookie", url_str);
            let url = url_str.parse().map_err(|e| format!("Invalid URL {}: {}", url_str, e))?;
        
            match window.cookies_for_url(url) {
                Ok(cookies) => {
                    log_info!("📋 从 {} 找到 {} 个cookie", url_str, cookies.len());
                    
                    // 查找 WorkosCursorSessionToken
                    for cookie in cookies {
                        log_info!("🍪 Cookie: {} = {}...", cookie.name(), &cookie.value()[..cookie.value().len().min(20)]);
                        
                        if cookie.name() == "WorkosCursorSessionToken" {
                            let token = cookie.value().to_string();
                            log_info!("✅ 找到 WorkosCursorSessionToken: {}...", &token[..token.len().min(50)]);
                            
                            // 发送事件到前端
                            let _ = app.emit("verification-login-cookie-found", serde_json::json!({
                                "WorkosCursorSessionToken": token
                            }));
                            
                            // 关闭窗口
                            if let Err(e) = window.close() {
                                log_error!("❌ 关闭验证码登录窗口失败: {}", e);
                            } else {
                                log_info!("✅ 验证码登录窗口已关闭");
                            }
                            
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    log_error!("❌ 从 {} 获取cookie失败: {}", url_str, e);
                }
            }
        }
        
        log_error!("❌ 未找到 WorkosCursorSessionToken");
        Err("未找到登录Token".to_string())
    } else {
        log_error!("❌ 未找到验证码登录窗口");
        Err("验证码登录窗口不存在".to_string())
    }
}

#[tauri::command]
async fn check_login_cookies(app: tauri::AppHandle) -> Result<(), String> {
    log_info!("🔍 开始检查登录Cookie");
    
    if let Some(window) = app.get_webview_window("auto_login") {
        // 尝试多个可能的URL来获取cookie
        let urls_to_try = vec![
            "https://authenticator.cursor.sh/",
            "https://cursor.com/",
            "https://app.cursor.com/",
            "https://www.cursor.com/",
        ];
        
        for url_str in urls_to_try {
            log_info!("🔍 尝试从 {} 获取cookie", url_str);
            let url = url_str.parse().map_err(|e| format!("Invalid URL {}: {}", url_str, e))?;
        
            match window.cookies_for_url(url) {
                Ok(cookies) => {
                    log_info!("📋 从 {} 找到 {} 个cookie", url_str, cookies.len());
                    
                    // 查找 WorkosCursorSessionToken
                    for cookie in cookies {
                        log_info!("🍪 Cookie: {} = {}...", cookie.name(), &cookie.value()[..cookie.value().len().min(20)]);
                        
                        if cookie.name() == "WorkosCursorSessionToken" {
                            let token = cookie.value().to_string();
                            log_info!("🎉 在 {} 找到 WorkosCursorSessionToken: {}...", url_str, &token[..token.len().min(20)]);
                            
                            // 关闭自动登录窗口
                            if let Err(e) = window.close() {
                                log_error!("❌ Failed to close auto login window: {}", e);
                            } else {
                                log_info!("✅ Auto login window closed successfully");
                            }
                            
                            // 发送事件通知前端获取到了token
                            if let Err(e) = app.emit("auto-login-success", serde_json::json!({
                                "token": token
                            })) {
                                log_error!("❌ Failed to emit auto login success event: {}", e);
                            } else {
                                log_info!("✅ Auto login success event emitted");
                            }
                            
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    log_error!("❌ 从 {} 获取cookie失败: {}", url_str, e);
                }
            }
        }
        
        // 如果所有URL都没找到目标cookie
        log_info!("⏳ 在所有URL中都未找到 WorkosCursorSessionToken");
        if let Err(e) = app.emit("auto-login-failed", serde_json::json!({
            "error": "未找到 WorkosCursorSessionToken cookie"
        })) {
            log_error!("❌ Failed to emit auto login failed event: {}", e);
        }
    } else {
        log_error!("❌ 未找到自动登录窗口");
        if let Err(e) = app.emit("auto-login-failed", serde_json::json!({
            "error": "未找到自动登录窗口"
        })) {
            log_error!("❌ Failed to emit auto login failed event: {}", e);
        }
    }
    
    Ok(())
}

#[tauri::command]
async fn auto_login_success(
    app: tauri::AppHandle,
    token: String,
) -> Result<(), String> {
    log_info!("🎉 自动登录成功，获取到Token: {}...", &token[..token.len().min(20)]);
    
    // 关闭自动登录窗口
    if let Some(window) = app.get_webview_window("auto_login") {
        if let Err(e) = window.close() {
            log_error!("❌ Failed to close auto login window: {}", e);
        } else {
            log_info!("✅ Auto login window closed successfully");
        }
    }
    
    // 发送事件通知前端获取到了token
    if let Err(e) = app.emit("auto-login-success", serde_json::json!({
        "token": token
    })) {
        log_error!("❌ Failed to emit auto login success event: {}", e);
    } else {
        log_info!("✅ Auto login success event emitted");
    }
    
    Ok(())
}

#[tauri::command]
async fn auto_login_failed(app: tauri::AppHandle, error: String) -> Result<(), String> {
    log_error!("❌ 自动登录失败: {}", error);
    
    // 关闭自动登录窗口
    if let Some(window) = app.get_webview_window("auto_login") {
        if let Err(e) = window.close() {
            log_error!("❌ Failed to close auto login window: {}", e);
        }
    }
    
    // 发送事件通知前端登录失败
    if let Err(e) = app.emit("auto-login-failed", serde_json::json!({
        "error": error
    })) {
        log_error!("❌ Failed to emit auto login failed event: {}", e);
    }
    
    Ok(())
}

#[tauri::command]
async fn open_cursor_dashboard(
    app: tauri::AppHandle,
    workos_cursor_session_token: String,
) -> Result<serde_json::Value, String> {
    log_info!("🔄 Opening Cursor dashboard with WorkOS token...");

    let url = "https://cursor.com/dashboard";

    // 先尝试关闭已存在的窗口
    if let Some(existing_window) = app.get_webview_window("cursor_dashboard") {
        log_info!("🔄 Closing existing cursor dashboard window...");
        if let Err(e) = existing_window.close() {
            log_error!("❌ Failed to close existing window: {}", e);
        } else {
            log_info!("✅ Existing window closed successfully");
        }
        // 等待一小段时间确保窗口完全关闭
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // 创建新的 WebView 窗口
    let webview_window = tauri::WebviewWindowBuilder::new(
        &app,
        "cursor_dashboard",
        tauri::WebviewUrl::External(url.parse().unwrap()),
    )
    .title("Cursor - 主页")
    .inner_size(1200.0, 800.0)
    .resizable(true)
    .initialization_script(&format!(
        r#"
        // 在页面加载前设置 Cookie
        document.cookie = 'WorkosCursorSessionToken={}; domain=.cursor.com; path=/; secure; samesite=none';
        console.log('Cookie injected for dashboard view');
        console.log('Current cookies:', document.cookie);
        "#,
        workos_cursor_session_token
    ))
    .visible(true)
    .build();

    match webview_window {
        Ok(window) => {
            // 添加窗口关闭事件监听器
            window.on_window_event(move |event| {
                match event {
                    tauri::WindowEvent::CloseRequested { .. } => {
                        log_info!("🔄 Cursor dashboard window close requested by user");
                    }
                    tauri::WindowEvent::Destroyed => {
                        log_info!("🔄 Cursor dashboard window destroyed");
                    }
                    _ => {}
                }
            });
            
            log_info!("✅ Successfully opened Cursor dashboard window");
            Ok(serde_json::json!({
                "success": true,
                "message": "已打开Cursor主页"
            }))
        }
        Err(e) => {
            log_error!("❌ Failed to create Cursor dashboard window: {}", e);
            Ok(serde_json::json!({
                "success": false,
                "message": format!("无法打开Cursor主页: {}", e)
            }))
        }
    }
}

#[tauri::command]
async fn show_auto_login_window(app: tauri::AppHandle) -> Result<(), String> {
    log_info!("🔍 Attempting to show auto login window");

    if let Some(window) = app.get_webview_window("auto_login") {
        window
            .show()
            .map_err(|e| format!("Failed to show auto login window: {}", e))?;
        log_info!("✅ Auto login window shown successfully");
    } else {
        log_error!("❌ Auto login window not found");
        return Err("Auto login window not found".to_string());
    }

    Ok(())
}


#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // 初始化日志系统
            if let Err(e) = logger::Logger::init() {
                eprintln!("Failed to initialize logger: {}", e);
            } else {
                log_info!("Application starting up...");
            }

            // 只在生产环境下复制 pyBuild 文件夹（macOS 和 Linux），开发模式下跳过
            if !cfg!(debug_assertions) && (cfg!(target_os = "macos") || cfg!(target_os = "linux")) {
                if let Err(e) = copy_pybuild_to_app_dir(app.handle()) {
                    log_error!("Failed to copy pyBuild directory on startup: {}", e);
                    // 不阻断应用启动，只记录错误
                }
            } else {
                if cfg!(debug_assertions) {
                    log_info!("Development mode detected, skipping pyBuild directory copy");
                } else {
                    log_info!("Windows platform detected, skipping pyBuild directory copy");
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            get_available_backups,
            extract_backup_ids,
            delete_backup,
            restore_machine_ids,
            get_cursor_paths,
            check_cursor_installation,
            reset_machine_ids,
            complete_cursor_reset,
            get_log_file_path,
            get_log_config,
            test_logging,
            debug_windows_cursor_paths,
            set_custom_cursor_path,
            get_custom_cursor_path,
            clear_custom_cursor_path,
            open_log_file,
            open_log_directory,
            get_current_machine_ids,
            get_machine_id_file_content,
            get_backup_directory_info,
            check_user_authorization,
            get_user_info,
            get_token_auto,
            debug_cursor_paths,
            get_account_list,
            add_account,
            edit_account,
            switch_account,
            switch_account_with_token,
            remove_account,
            logout_current_account,
            export_accounts,
            import_accounts,
            open_cancel_subscription_page,
            show_cancel_subscription_window,
            cancel_subscription_failed,
            open_manual_bind_card_page,
            get_bind_card_url,
            show_manual_bind_card_window,
            manual_bind_card_failed,
            delete_cursor_account,
            trigger_authorization_login,
            trigger_authorization_login_poll,
            get_usage_for_period,
            get_user_analytics,
            get_usage_events,
            register_cursor_account,
            create_temp_email,
            register_with_email,
            batch_register_with_email,
            register_with_cloudflare_temp_email,
            register_with_outlook,
            submit_verification_code,
            cancel_registration,
            get_saved_accounts,
            read_bank_card_config,
            save_bank_card_config,
            read_email_config,
            save_email_config,
            get_app_version,
            open_update_url,
            copy_pybuild_resources,
            auto_login_and_get_cookie,
            check_login_cookies,
            auto_login_success,
            auto_login_failed,
            show_auto_login_window,
            open_cursor_dashboard,
            verification_code_login,
            check_verification_login_cookies,
            generate_virtual_card
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// 生成虚拟卡
#[tauri::command]
async fn generate_virtual_card(
    cdk_code: String,
    custom_prefix: String,
) -> Result<serde_json::Value, String> {
    log_info!("🔄 开始生成虚拟卡...");
    log_info!("📧 CDK码: {}", cdk_code);
    log_info!("💳 卡头: {}", custom_prefix);

    let client = reqwest::Client::new();
    
    let request_body = serde_json::json!({
        "cdkCode": cdk_code,
        "count": 1,
        "customPrefix": custom_prefix
    });

    log_info!("📤 请求体: {}", request_body);

    match client
        .post("https://api.anify.cn/virtual-card/generate")
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            log_info!("📥 响应状态: {}", status);

            if status.is_success() {
                match response.json::<serde_json::Value>().await {
                    Ok(data) => {
                        log_info!("✅ 虚拟卡生成成功");
                        Ok(data)
                    }
                    Err(e) => {
                        log_error!("❌ 解析响应失败: {}", e);
                        Err(format!("解析响应失败: {}", e))
                    }
                }
            } else {
                let error_text = response.text().await.unwrap_or_else(|_| "未知错误".to_string());
                log_error!("❌ API请求失败: {} - {}", status, error_text);
                Err(error_text)
            }
        }
        Err(e) => {
            log_error!("❌ 网络请求失败: {}", e);
            Err(format!("网络请求失败: {}", e))
        }
    }
}
