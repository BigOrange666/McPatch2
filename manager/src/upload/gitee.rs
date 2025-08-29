use std::path::PathBuf;
use std::time::Duration;

use base64ct::{Base64, Encoding};
use reqwest::Client;
use serde_json::json;

use crate::config::gitee_config::GiteeConfig;
use crate::upload::UploadTarget;
use crate::utility::to_detail_error::ToDetailError;

pub struct GiteeTarget {
    config: GiteeConfig,
    client: Client,
}

impl GiteeTarget {
    pub async fn new(config: GiteeConfig) -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(30000))  // 30秒连接超时
            .read_timeout(Duration::from_millis(60000))     // 60秒读取超时
            .user_agent("mcpatch-gitee-uploader/1.0")
            .build()
            .unwrap();

        Self {
            config,
            client,
        }
    }

    /// 获取文件的SHA值，用于更新文件
    async fn get_file_sha(&self, filepath: &str) -> Result<Option<String>, String> {
        let full_path = if self.config.path.is_empty() {
            filepath.to_string()
        } else {
            format!("{}/{}", self.config.path.trim_end_matches('/'), filepath)
        };

        let url = format!(
            "https://gitee.com/api/v5/repos/{}/{}/contents/{}",
            self.config.username, self.config.repo, full_path
        );

        println!("Gitee: Getting SHA for file: {} (full path: {})", filepath, full_path);
        println!("Gitee: Request URL: {}", url);

        let response = self.client
            .get(&url)
            .header("Authorization", format!("token {}", self.config.token))
            .send()
            .await
            .map_err(|e| {
                println!("Gitee: Failed to send request: {}", e);
                e.to_detail_error()
            })?;

        println!("Gitee: Response status: {}", response.status());

        if response.status().is_success() {
            let json: serde_json::Value = response
                .json()
                .await
                .map_err(|e| {
                    println!("Gitee: Failed to parse JSON response: {}", e);
                    e.to_detail_error()
                })?;
            
            println!("Gitee: Response JSON: {:?}", json);
            
            // 检查是否是空数组（文件不存在的情况）
            if let Some(array) = json.as_array() {
                if array.is_empty() {
                    println!("Gitee: File not found (empty array response)");
                    Ok(None)
                } else {
                    println!("Gitee: Unexpected array response with {} items", array.len());
                    Ok(None)
                }
            } else if let Some(sha) = json.get("sha").and_then(|v| v.as_str()) {
                println!("Gitee: Found SHA: {}", sha);
                Ok(Some(sha.to_string()))
            } else {
                println!("Gitee: No SHA found in response");
                Ok(None)
            }
        } else if response.status().as_u16() == 404 {
            println!("Gitee: File not found (404)");
            Ok(None)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_else(|e| {
                println!("Gitee: Failed to read response text: {}", e);
                "Unknown error".to_string()
            });
            println!("Gitee: Failed to get file SHA: {} - {}", status, text);
            Err(format!("Failed to get file SHA: {} - {}", status, text))
        }
    }
}

impl UploadTarget for GiteeTarget {
    async fn list(&mut self) -> Result<Vec<(String, u64)>, String> {
        let list_path = if self.config.path.is_empty() {
            "".to_string()
        } else {
            self.config.path.trim_end_matches('/').to_string()
        };

        let url = if list_path.is_empty() {
            format!(
                "https://gitee.com/api/v5/repos/{}/{}/contents",
                self.config.username, self.config.repo
            )
        } else {
            format!(
                "https://gitee.com/api/v5/repos/{}/{}/contents/{}",
                self.config.username, self.config.repo, list_path
            )
        };

        let response = self.client
            .get(&url)
            .header("Authorization", format!("token {}", self.config.token))
            .send()
            .await
            .map_err(|e| e.to_detail_error())?;

        if response.status().is_success() {
            let json: serde_json::Value = response
                .json()
                .await
                .map_err(|e| e.to_detail_error())?;

            let mut files = Vec::new();
            
            // contents API返回数组（文件夹内容）
            if let Some(items) = json.as_array() {
                for item in items {
                    if let (Some(name), Some(item_type)) = (
                        item.get("name").and_then(|v| v.as_str()),
                        item.get("type").and_then(|v| v.as_str()),
                    ) {
                        if item_type == "file" {
                            files.push((name.to_string(), std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs()));
                        }
                    }
                }
            }

            Ok(files)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            Err(format!("Failed to list files: {} - {}", status, text))
        }
    }

    async fn read(&mut self, filename: &str) -> Result<Option<String>, String> {
        let full_path = if self.config.path.is_empty() {
            filename.to_string()
        } else {
            format!("{}/{}", self.config.path.trim_end_matches('/'), filename)
        };

        let url = format!(
            "https://gitee.com/api/v5/repos/{}/{}/contents/{}",
            self.config.username, self.config.repo, full_path
        );

        let response = self.client
            .get(&url)
            .header("Authorization", format!("token {}", self.config.token))
            .send()
            .await
            .map_err(|e| e.to_detail_error())?;

        if response.status().is_success() {
            let json: serde_json::Value = response
                .json()
                .await
                .map_err(|e| e.to_detail_error())?;

            if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
                // Gitee API 返回的是base64编码的内容
                let decoded = Base64::decode_vec(content)
                    .map_err(|e| format!("Failed to decode base64 content: {}", e))?;
                let text = String::from_utf8(decoded)
                    .map_err(|e| format!("Failed to convert to UTF-8: {}", e))?;
                Ok(Some(text))
            } else {
                Ok(None)
            }
        } else if response.status().as_u16() == 404 {
            Ok(None)
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            Err(format!("Failed to read file: {} - {}", status, text))
        }
    }

    async fn write(&mut self, filename: &str, content: &str) -> Result<(), String> {
        let full_path = if self.config.path.is_empty() {
            filename.to_string()
        } else {
            format!("{}/{}", self.config.path.trim_end_matches('/'), filename)
        };

        println!("Gitee: Writing file: {} to path: {}", filename, full_path);
        println!("Gitee: Content size: {} bytes", content.len());

        // 先获取文件的SHA值（如果文件存在）
        println!("Gitee: Getting SHA for existing file...");
        let sha = self.get_file_sha(filename).await?;
        println!("Gitee: SHA result: {:?}", sha);

        let url = format!(
            "https://gitee.com/api/v5/repos/{}/{}/contents/{}",
            self.config.username, self.config.repo, full_path
        );

        println!("Gitee: Write URL: {}", url);

        let payload = json!({
            "access_token": self.config.token,
            "content": Base64::encode_string(content.as_bytes()),
            "message": format!("Update {}", filename),
            "branch": self.config.branch,
        });

        // 如果文件存在，添加SHA值用于更新
        let payload = if let Some(ref sha_value) = sha {
            let mut payload_obj = payload.as_object().unwrap().clone();
            payload_obj.insert("sha".to_string(), serde_json::Value::String(sha_value.clone()));
            println!("Gitee: Updating existing file with SHA: {}", sha_value);
            serde_json::Value::Object(payload_obj)
        } else {
            println!("Gitee: Creating new file");
            payload
        };

        println!("Gitee: Sending write request...");

        let response = if sha.is_some() {
            // 更新现有文件使用PUT方法
            self.client
                .put(&url)
                .json(&payload)
                .send()
                .await
                .map_err(|e| {
                    println!("Gitee: Failed to send write request: {}", e);
                    e.to_detail_error()
                })?
        } else {
            // 创建新文件使用POST方法
            self.client
                .post(&url)
                .json(&payload)
                .send()
                .await
                .map_err(|e| {
                    println!("Gitee: Failed to send write request: {}", e);
                    e.to_detail_error()
                })?
        };

        println!("Gitee: Write response status: {}", response.status());

        if response.status().is_success() {
            println!("Gitee: Write successful!");
            Ok(())
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_else(|e| {
                println!("Gitee: Failed to read response text: {}", e);
                "Unknown error".to_string()
            });
            println!("Gitee: Write failed: {} - {}", status, text);
            Err(format!("Failed to write file: {} - {}", status, text))
        }
    }

    async fn upload(&mut self, filename: &str, filepath: PathBuf) -> Result<(), String> {
        let full_path = if self.config.path.is_empty() {
            filename.to_string()
        } else {
            format!("{}/{}", self.config.path.trim_end_matches('/'), filename)
        };

        println!("Gitee: Uploading file: {} to path: {}", filename, full_path);
        println!("Gitee: File path: {:?}", filepath);

        let content = tokio::fs::read(&filepath)
            .await
            .map_err(|e| {
                println!("Gitee: Failed to read file: {}", e);
                e.to_detail_error()
            })?;

        println!("Gitee: File size: {} bytes", content.len());

        // 先获取文件的SHA值（如果文件存在）
        println!("Gitee: Getting SHA for existing file...");
        let sha = self.get_file_sha(filename).await?;
        println!("Gitee: SHA result: {:?}", sha);

        let url = format!(
            "https://gitee.com/api/v5/repos/{}/{}/contents/{}",
            self.config.username, self.config.repo, full_path
        );

        println!("Gitee: Upload URL: {}", url);

        let payload = json!({
            "access_token": self.config.token,
            "content": Base64::encode_string(&content),
            "message": format!("Upload {} for version {}", filename, filename),
            "branch": self.config.branch,
        });

        // 如果文件存在，添加SHA值用于更新
        let payload = if let Some(ref sha_value) = sha {
            let mut payload_obj = payload.as_object().unwrap().clone();
            payload_obj.insert("sha".to_string(), serde_json::Value::String(sha_value.clone()));
            println!("Gitee: Updating existing file with SHA: {}", sha_value);
            serde_json::Value::Object(payload_obj)
        } else {
            println!("Gitee: Creating new file");
            payload
        };

        println!("Gitee: Sending upload request...");

        let response = if sha.is_some() {
            // 更新现有文件使用PUT方法
            self.client
                .put(&url)
                .json(&payload)
                .send()
                .await
                .map_err(|e| {
                    println!("Gitee: Failed to send upload request: {}", e);
                    e.to_detail_error()
                })?
        } else {
            // 创建新文件使用POST方法
            self.client
                .post(&url)
                .json(&payload)
                .send()
                .await
                .map_err(|e| {
                    println!("Gitee: Failed to send upload request: {}", e);
                    e.to_detail_error()
                })?
        };

        println!("Gitee: Upload response status: {}", response.status());

        if response.status().is_success() {
            println!("Gitee: Upload successful!");
            Ok(())
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_else(|e| {
                println!("Gitee: Failed to read response text: {}", e);
                "Unknown error".to_string()
            });
            println!("Gitee: Upload failed: {} - {}", status, text);
            Err(format!("Failed to upload file: {} - {}", status, text))
        }
    }

    async fn delete(&mut self, filename: &str) -> Result<(), String> {
        let full_path = if self.config.path.is_empty() {
            filename.to_string()
        } else {
            format!("{}/{}", self.config.path.trim_end_matches('/'), filename)
        };

        println!("Gitee: Deleting file: {} from path: {}", filename, full_path);

        // 先获取文件的SHA值
        println!("Gitee: Getting SHA for file to delete...");
        let sha = match self.get_file_sha(filename).await? {
            Some(sha) => {
                println!("Gitee: Found SHA for deletion: {}", sha);
                sha
            },
            None => {
                println!("Gitee: File not found, nothing to delete");
                return Ok(()); // 文件不存在，无需删除
            }
        };

        let url = format!(
            "https://gitee.com/api/v5/repos/{}/{}/contents/{}",
            self.config.username, self.config.repo, full_path
        );

        println!("Gitee: Delete URL: {}", url);

        let payload = json!({
            "access_token": self.config.token,
            "sha": sha,
            "message": format!("Delete {}", filename),
            "branch": self.config.branch,
        });

        println!("Gitee: Sending delete request...");

        let response = self.client
            .delete(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                println!("Gitee: Failed to send delete request: {}", e);
                e.to_detail_error()
            })?;

        println!("Gitee: Delete response status: {}", response.status());

        if response.status().is_success() {
            println!("Gitee: Delete successful!");
            Ok(())
        } else {
            let status = response.status();
            let text = response.text().await.unwrap_or_else(|e| {
                println!("Gitee: Failed to read response text: {}", e);
                "Unknown error".to_string()
            });
            println!("Gitee: Delete failed: {} - {}", status, text);
            Err(format!("Failed to delete file: {} - {}", status, text))
        }
    }
}