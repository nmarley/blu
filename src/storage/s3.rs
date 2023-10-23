// TODO: remove these when s3 adapter is complete/ready to use
#![allow(dead_code)]
#![allow(unused_variables)]
use aws_sdk_s3::operation::{
    get_object::{GetObjectError, GetObjectOutput},
    put_object::{PutObjectError, PutObjectOutput},
};
use aws_sdk_s3::{error::SdkError, primitives::ByteStream};
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use std::path::{Path, PathBuf};
use tokio_stream::StreamExt;

use crate::hash::Hash;

use super::StorageBackend;

/// Amazon S3 storage backend
#[derive(Debug)]
pub struct AmazonS3 {
    bucket: String,
    prefix: PathBuf,
    client: aws_sdk_s3::Client,
}

impl AmazonS3 {
    /// Create a new Amazon S3 storage backend with the given bucket name and
    /// optional prefix.
    pub fn new<P: AsRef<Path>>(bucket: &str, prefix: Option<P>) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (config, client) = runtime.block_on(async {
            let config = aws_config::load_from_env().await;
            let client = aws_sdk_s3::Client::new(&config);
            (config, client)
        });

        let prefix = match prefix {
            Some(ref p) => p.as_ref().to_path_buf(),
            None => Path::new("").to_path_buf(),
        };
        info!("prefix = {}", prefix.display());

        Self {
            bucket: bucket.to_owned(),
            prefix,
            client,
        }
    }
}

impl StorageBackend for AmazonS3 {
    fn read_data(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let key = self.prefix.join(path);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut object = runtime.block_on(async {
            let object = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(key.to_string_lossy().to_string())
                .send()
                .await?;
            Ok::<GetObjectOutput, SdkError<GetObjectError, HttpResponse>>(object)
        })?;

        let (buf, byte_count) = runtime.block_on(async {
            let mut buf: Vec<u8> = vec![];
            let mut byte_count = 0_usize;
            while let Some(bytes) = object.body.try_next().await? {
                buf.extend_from_slice(&bytes);
                byte_count += bytes.len();
            }
            Ok::<(Vec<u8>, usize), Box<dyn std::error::Error>>((buf, byte_count))
        })?;

        Ok(buf)
    }

    fn write_data(&self, hash: &Hash, data: &[u8]) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = super::path_for(hash)?;
        let key = self.prefix.join(&path);
        info!("key = {}", key.display());

        let body = ByteStream::from(data.to_vec());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut _put_obj_output = runtime.block_on(async {
            let put_obj_output = self
                .client
                .put_object()
                .bucket(&self.bucket)
                .key(key.to_string_lossy().to_string())
                .body(body)
                .send()
                .await?;
            Ok::<PutObjectOutput, SdkError<PutObjectError, HttpResponse>>(put_obj_output)
        })?;

        Ok(path)
    }
}
