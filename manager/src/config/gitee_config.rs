use serde::Deserialize;
use serde::Serialize;

/// gitee上传的配置
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default, rename_all = "kebab-case")]
pub struct GiteeConfig {
    /// 启用gitee的上传
    pub enabled: bool,

    /// gitee用户名
    pub username: String,

    /// gitee访问令牌
    pub token: String,

    /// 仓库名称
    pub repo: String,

    /// 分支名称
    pub branch: String,

    /// 仓库路径（文件夹路径）
    pub path: String,
}