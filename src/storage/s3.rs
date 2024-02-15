use async_trait::async_trait;
use aws_sdk_s3::{primitives::ByteStream, Client, Error};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::hash::Hash;

use super::StorageBackend;
use super::StorageError;

/// Amazon S3 storage backend
#[derive(Clone, Debug)]
pub struct AmazonS3 {
    bucket: String,
    prefix: Option<PathBuf>,
    client: Client,
}

impl AmazonS3 {
    /// Create a new Amazon S3 storage backend with the given bucket name and
    /// optional prefix.
    pub async fn new(bucket: &str, prefix: Option<&str>) -> Self {
        let config = aws_config::load_from_env().await;
        let client = Client::new(&config);

        Self {
            bucket: bucket.to_owned(),
            prefix: prefix.map(|e| PathBuf::from(e.to_owned())),
            client,
        }
    }

    /// Get an object from the S3 bucket
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

    /// Get an object async read stream from the S3 bucket
    pub async fn get_object_stream(
        &self,
        key: &str,
    ) -> Result<Box<dyn AsyncRead + Unpin + Send>, Error> {
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

        Ok(Box::new(ByteStream::into_async_read(object.body)))
    }

    /// Put an object into the S3 bucket
    pub async fn put_object(&self, key: &str, data: &[u8]) -> Result<(), Error> {
        let key = &(match self.prefix {
            Some(ref prefix) => Path::new(&prefix)
                .join(key)
                .into_os_string()
                .into_string()
                .unwrap(),
            None => key.to_owned(),
        });

        // Result<PutObjectOutput, SdkError<PutObjectError>>
        let _ = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(data.to_vec()))
            .send()
            .await?;

        Ok(())
    }
}

#[async_trait]
impl StorageBackend for AmazonS3 {
    async fn read_data(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Unpin + Send>, StorageError> {
        let key = path.to_string_lossy().to_string();
        let data = match self.get_object_stream(&key).await {
            Ok(data) => data,
            Err(_e) => return Err(StorageError::ReadError(key.into())),
        };
        Ok(data)
    }

    async fn write_data(&self, hash: &Hash, data: &[u8]) -> Result<PathBuf, StorageError> {
        let path = match super::path_for(hash) {
            Ok(path) => path,
            Err(err) => return Err(StorageError::HashError(err)),
        };
        let key = path.to_string_lossy().to_string();
        info!("key = {:?}", key);

        match self.put_object(&key, data).await {
            Ok(_) => (),
            Err(_e) => return Err(StorageError::WriteError(key.into())),
        };
        Ok(path)
    }
}

// impl S3Agent {
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
