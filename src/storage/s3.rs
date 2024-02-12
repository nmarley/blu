use async_trait::async_trait;
use aws_sdk_s3::operation::{
    get_object::{GetObjectError, GetObjectOutput},
    put_object::{PutObjectError, PutObjectOutput},
};
use aws_sdk_s3::{error::SdkError, primitives::ByteStream};
use aws_sdk_s3::{Client, Error};
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use std::path::{Path, PathBuf};
// use tokio_stream::StreamExt;
// NEW v
use tokio::io::AsyncReadExt;
// use aws_sdk_s3::primitives::ByteStream;

use crate::hash::Hash;

use super::StorageBackend;

/// Amazon S3 storage backend
#[derive(Debug)]
pub struct AmazonS3 {
    bucket: String,
    prefix: PathBuf,
    client: Client,
}

impl AmazonS3 {
    /// Create a new Amazon S3 storage backend with the given bucket name and
    /// optional prefix.
    pub async fn new<P: AsRef<Path>>(bucket: &str, prefix: Option<P>) -> Self {
        let config = aws_config::load_from_env().await;
        let client = Client::new(&config);

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

    pub async fn get_object(&self, key: &str) -> Result<Vec<u8>, Error> {
        let key = &(match self.prefix {
            Some(ref prefix) => Path::new(&prefix)
                .join(key)
                .into_os_string()
                .into_string()
                .unwrap(),
            None => key.to_owned(),
        });

        let object = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await?;

        let mut data = Vec::new();
        let mut stream = ByteStream::into_async_read(object.body);
        // Read the stream into the vec
        stream.read_to_end(&mut data).await.unwrap();

        Ok(data)
    }
}

#[async_trait]
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

        let (buf, _byte_count) = runtime.block_on(async {
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

    async fn async_read_data(&self, path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let key = self.prefix.join(path);

        let object = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key.to_string_lossy().to_string())
            .send()
            .await?;

        let mut buf: Vec<u8> = vec![];
        while let Some(bytes) = object.body.try_next().await? {
            buf.extend_from_slice(&bytes);
        }

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

// s3agent.rs
// use aws_sdk_s3::primitives::ByteStream;
// use aws_sdk_s3::{Client, Error};
// use std::path::Path;
// use tokio::io::AsyncReadExt;
//
// #[derive(Clone, Debug)]
// pub struct S3Agent {
//     bucket: String,
//     prefix: Option<String>,
//     client: Client,
// }
//
// // see:
// // https://docs.aws.amazon.com/sdk-for-rust/latest/dg/rust_s3_code_examples.html
//
// impl S3Agent {
//     pub async fn new(bucket: &str, prefix: Option<&str>) -> Self {
//         let config = aws_config::load_from_env().await;
//         let client = aws_sdk_s3::Client::new(&config);
//
//         Self {
//             bucket: bucket.to_owned(),
//             prefix: prefix.map(|e| e.to_owned()),
//             client,
//         }
//     }
//
//     pub async fn get_object(&self, key: &str) -> Result<Vec<u8>, Error> {
//         let key = &(match self.prefix {
//             Some(ref prefix) => Path::new(&prefix)
//                 .join(key)
//                 .into_os_string()
//                 .into_string()
//                 .unwrap(),
//             None => key.to_owned(),
//         });
//
//         let object = self
//             .client
//             .get_object()
//             .bucket(&self.bucket)
//             .key(key)
//             .send()
//             .await?;
//
//         let mut data = Vec::new();
//         let mut stream = ByteStream::into_async_read(object.body);
//         // Read the stream into the vec
//         stream.read_to_end(&mut data).await.unwrap();
//
//         Ok(data)
//     }
//
//     // note: This was added as a sanity check, ensure we can see the bucket
//     // before trying to download a shit-ton of files... or handle 'NoSuchBucket'
//     // error and abort if we get one upon trying to get_object
//
//     #[allow(dead_code)]
//     /// Example method to list all buckets, needs s3 iam permission
//     pub async fn list_buckets(&self) -> Result<Vec<String>, aws_sdk_s3::Error> {
//         let resp = self.client.list_buckets().send().await?;
//         let buckets: Vec<String> = resp
//             .buckets()
//             .iter()
//             .map(|e| e.name().unwrap().to_string())
//             .collect();
//         Ok(buckets)
//     }
// }
