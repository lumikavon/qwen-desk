use serde::Serialize;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Clone, Serialize)]
pub struct AppError { pub code: String, pub message: String }
impl AppError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self { Self { code: code.into(), message: message.into() } }
    pub fn invalid(message: impl Into<String>) -> Self { Self::new("invalid_request", message) }
    pub fn io() -> Self { Self::new("file_error", "文件操作失败，请检查权限及可用磁盘空间。") }
}
impl std::fmt::Display for AppError { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}: {}", self.code, self.message) } }
impl std::error::Error for AppError {}
