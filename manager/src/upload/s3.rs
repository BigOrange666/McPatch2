use core::str;
use std::path::PathBuf;

use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::config::BehaviorVersion;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::CompletedMultipartUpload;
use aws_sdk_s3::types::CompletedPart;
use aws_sdk_s3::Client;
use tokio::io::AsyncReadExt;

use crate::config::s3_config::S3Config;
use crate::upload::UploadTarget;
use crate::utility::to_detail_error::ToDetailError;

/// S3 分块上传的最小分块大小（除最后一个分块外，每个分块必须 >= 5MB）
const MIN_PART_SIZE: u64 = 5 * 1024 * 1024;

/// S3 分块上传的分块大小
const PART_SIZE: usize = 8 * 1024 * 1024;

pub struct S3Target {
    config: S3Config,
    client: Client,
}

impl S3Target {
    pub async fn new(config: S3Config) -> Self {
        let cfg = aws_sdk_s3::config::Builder::new()
            .endpoint_url(config.endpoint.clone())
            .region(Region::new(config.region.clone()))
            .behavior_version(BehaviorVersion::v2024_03_28())
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                config.access_id.clone(),
                config.secret_key.clone(),
                None,
                None,
                "mcpatch-provider"
            ))
            .build();

        let client = aws_sdk_s3::Client::from_conf(cfg);

        Self {
            config,
            client,
        }
    }
}

impl UploadTarget for S3Target {
    async fn list(&mut self) -> Result<Vec<(String, u64)>, String> {
        println!("[S3] 开始获取文件列表: bucket={}", self.config.bucket);

        let list_rsp = match self.client
            .list_objects()
            .bucket(&self.config.bucket)
            .send()
            .await
        {
            Ok(rsp) => rsp,
            Err(e) => {
                println!("[S3] 获取文件列表失败: error={:#?}", e);
                if let Some(service_err) = e.as_service_error() {
                    println!("[S3] 获取文件列表-服务端错误: code={:?}, message={:?}",
                        service_err.meta().code(), service_err.message());
                }
                return Err(format!("[S3] 获取文件列表失败: {}", e.to_detail_error()));
            }
        };

        let result: Vec<(String, u64)> = list_rsp.contents().iter().map(|e| (e.key().unwrap().to_owned(), e.last_modified().unwrap().secs() as u64)).collect();
        println!("[S3] 获取文件列表成功: 共 {} 个文件", result.len());

        Ok(result)
    }
    
    async fn read(&mut self, filename: &str) -> Result<Option<String>, String> {
        println!("[S3] 开始读取文件: filename={}", filename);

        let result = self.client.get_object()
            .bucket(&self.config.bucket)
            .key(filename)
            .send()
            .await;

        let read = match result {
            Ok(ok) => ok,
            Err(err) => {
                if let Some(e) = err.as_service_error() {
                    if e.is_no_such_key() {
                        println!("[S3] 读取文件: 文件不存在, filename={}", filename);
                        return Ok(None);
                    }
                }
    
                println!("[S3] 读取文件失败: filename={}, error={:#?}", filename, err);
                if let Some(service_err) = err.as_service_error() {
                    println!("[S3] 读取文件-服务端错误: code={:?}, message={:?}",
                        service_err.meta().code(), service_err.message());
                }
                return Err(format!("[S3] 读取文件失败: {}", err.to_detail_error()));
            },
        };

        let body = read.body.collect().await.unwrap();
        let bytes = body.into_bytes();
        let text = std::str::from_utf8(&bytes).unwrap().to_owned();

        println!("[S3] 读取文件成功: filename={}", filename);
        Ok(Some(text))
    }
    
    async fn write(&mut self, filename: &str, content: &str) -> Result<(), String> {
        println!("[S3] 开始写入文件: filename={}", filename);

        match self.client.put_object()
            .bucket(&self.config.bucket)
            .key(filename)
            .body(ByteStream::from(content.as_bytes().to_vec()))
            .send()
            .await
        {
            Ok(_) => {
                println!("[S3] 写入文件成功: filename={}", filename);
                Ok(())
            },
            Err(e) => {
                println!("[S3] 写入文件失败: filename={}, error={:#?}", filename, e);
                if let Some(service_err) = e.as_service_error() {
                    println!("[S3] 写入文件-服务端错误: code={:?}, message={:?}",
                        service_err.meta().code(), service_err.message());
                }
                Err(format!("[S3] 写入文件失败: {}", e.to_detail_error()))
            }
        }
    }
    
    async fn upload(&mut self, filename: &str, filepath: PathBuf) -> Result<(), String> {
        println!("[S3] 开始上传文件: filename={}, filepath={}", filename, filepath.to_str().unwrap_or("未知"));

        let metadata = match tokio::fs::metadata(&filepath).await {
            Ok(m) => m,
            Err(e) => {
                println!("[S3] 上传失败: 无法获取文件信息 {}, error={:#?}", filepath.to_str().unwrap_or("未知"), e);
                return Err(format!("[S3] 上传失败-无法获取文件信息: {}", e.to_detail_error()));
            }
        };
        let file_size = metadata.len();
        println!("[S3] 上传: 文件大小 = {} 字节", file_size);

        let file = match tokio::fs::File::open(&filepath).await {
            Ok(f) => f,
            Err(e) => {
                println!("[S3] 上传失败: 无法打开文件 {}, error={:#?}", filepath.to_str().unwrap_or("未知"), e);
                return Err(format!("[S3] 上传失败-无法打开文件: {}", e.to_detail_error()));
            }
        };

        // 小文件（< 5MB）直接 put_object 单次上传，避免单分块不满足最小分块大小限制
        if file_size < MIN_PART_SIZE {
            println!("[S3] 上传: 文件小于 5MB，使用单次上传...");

            let bytes = match tokio::fs::read(&filepath).await {
                Ok(b) => b,
                Err(e) => {
                    println!("[S3] 上传失败: 读取文件内容失败, error={:#?}", e);
                    return Err(format!("[S3] 上传失败-读取文件内容: {}", e.to_detail_error()));
                }
            };

            match self.client.put_object()
                .bucket(&self.config.bucket)
                .key(filename)
                .body(ByteStream::from(bytes))
                .send()
                .await
            {
                Ok(_) => {
                    println!("[S3] 上传成功（单次上传）: filename={}", filename);
                    Ok(())
                },
                Err(e) => {
                    println!("[S3] 上传失败: 单次上传出错, error={:#?}", e);
                    if let Some(service_err) = e.as_service_error() {
                        println!("[S3] 上传-服务端错误: code={:?}, message={:?}",
                            service_err.meta().code(), service_err.message());
                    }
                    Err(format!("[S3] 上传失败-单次上传: {}", e.to_detail_error()))
                }
            }
        } else {
            // 大文件使用分块上传
            let mut file = tokio::io::BufReader::new(file);

            // 准备分块上传
            println!("[S3] 上传: 创建分块上传任务...");
            let create_result = self.client
                .create_multipart_upload()
                .bucket(&self.config.bucket)
                .key(filename)
                .send()
                .await;

            let (upload_id, complete_parts) = match create_result {
                Ok(rsp) => {
                    println!("[S3] 上传: 创建分块上传任务成功, upload_id={:?}", rsp.upload_id.as_deref());
                    let upload_id = rsp.upload_id.unwrap();

                    let mut part_number = 1;
                    let mut complete_parts = CompletedMultipartUpload::builder();
                    let mut buffer = vec![0; PART_SIZE];

                    // 分块上传
                    let mut uploaded = 0;

                    while uploaded < file_size {
                        // 计算本次应读取的字节数：除最后一块外固定为 PART_SIZE
                        let remaining = (file_size - uploaded) as usize;
                        let read_size = if remaining >= PART_SIZE {
                            PART_SIZE
                        } else {
                            remaining
                        };

                        // 使用 read_exact 保证恰好读满，避免产生小于 5MB 的中间分块
                        match file.read_exact(&mut buffer[..read_size]).await {
                            Ok(_) => {},
                            Err(e) => {
                                println!("[S3] 上传失败: 第 {} 块读取本地文件失败, error={:#?}", part_number, e);
                                return Err(format!("[S3] 上传失败-第 {} 块读取本地文件: {}", part_number, e.to_detail_error()));
                            }
                        }

                        println!("[S3] 上传: 正在上传第 {} 块, 偏移={}, 大小={}", part_number, uploaded, read_size);

                        // 上传当前块
                        let body = ByteStream::from(buffer[..read_size].to_vec());

                        let part_result = self.client
                            .upload_part()
                            .bucket(&self.config.bucket)
                            .key(filename)
                            .part_number(part_number)
                            .upload_id(upload_id.clone())
                            .body(body)
                            .send()
                            .await;

                        match part_result {
                            Ok(rsp) => {
                                println!("[S3] 上传: 第 {} 块上传成功, etag={:?}", part_number, rsp.e_tag.as_deref());
                                let cp = CompletedPart::builder()
                                    .part_number(part_number)
                                    .e_tag(rsp.e_tag.unwrap())
                                    .build();
                                complete_parts = complete_parts.parts(cp);

                                uploaded += read_size as u64;
                                part_number += 1;
                            },
                            Err(e) => {
                                println!("[S3] 上传失败: 第 {} 块上传出错, error={:#?}", part_number, e);
                                if let Some(service_err) = e.as_service_error() {
                                    println!("[S3] 上传-服务端错误: code={:?}, message={:?}",
                                        service_err.meta().code(), service_err.message());
                                }
                                return Err(format!("[S3] 上传失败-第 {} 块上传: {}", part_number, e.to_detail_error()));
                            }
                        }
                    }

                    (upload_id, complete_parts.build())
                },
                Err(e) => {
                    println!("[S3] 上传失败: 创建分块上传任务出错, error={:#?}", e);
                    if let Some(service_err) = e.as_service_error() {
                        println!("[S3] 上传-服务端错误: code={:?}, message={:?}",
                            service_err.meta().code(), service_err.message());
                    }
                    return Err(format!("[S3] 上传失败-创建分块上传任务: {}", e.to_detail_error()));
                }
            };

            // 结束上传
            println!("[S3] 上传: 正在完成分块上传...");
            match self.client
                .complete_multipart_upload()
                .bucket(&self.config.bucket)
                .key(filename)
                .upload_id(upload_id)
                .multipart_upload(complete_parts)
                .send()
                .await
            {
                Ok(_) => {
                    println!("[S3] 上传成功: filename={}", filename);
                    Ok(())
                },
                Err(e) => {
                    println!("[S3] 上传失败: 完成分块上传出错, error={:#?}", e);
                    if let Some(service_err) = e.as_service_error() {
                        println!("[S3] 上传-服务端错误: code={:?}, message={:?}",
                            service_err.meta().code(), service_err.message());
                    }
                    Err(format!("[S3] 上传失败-完成分块上传: {}", e.to_detail_error()))
                }
            }
        }
    }
    
    async fn delete(&mut self, filename: &str) -> Result<(), String> {
        println!("[S3] 开始删除文件: filename={}", filename);

        match self.client
            .delete_object()
            .bucket(&self.config.bucket)
            .key(filename)
            .send()
            .await
        {
            Ok(_) => {
                println!("[S3] 删除文件成功: filename={}", filename);
                Ok(())
            },
            Err(e) => {
                println!("[S3] 删除文件失败: filename={}, error={:#?}", filename, e);
                if let Some(service_err) = e.as_service_error() {
                    println!("[S3] 删除文件-服务端错误: code={:?}, message={:?}",
                        service_err.meta().code(), service_err.message());
                }
                Err(format!("[S3] 删除文件失败: {}", e.to_detail_error()))
            }
        }
    }
}
