//! OpenDAL 的受限 HTTP transport；OSS 数据面在发送前统一改签 V4。

use std::time::Duration;

use http::header::{AUTHORIZATION, DATE};
use opendal::{
    Buffer, Error, ErrorKind, HttpBody, HttpTransport, HttpTransporter, OperationContext,
};
use opendal_http_transport_reqwest::ReqwestTransport;
use reqsign_aliyun_oss::{RequestSigner, SigningVersion, StaticCredentialProvider};
use reqsign_core::{Context, Signer};

use ramag_domain::entities::{CloudProvider, ObjectStorageAccountSnapshot, ObjectStorageMount};
use ramag_domain::error::ObjectStorageResult;

use crate::errors::invalid;

#[derive(Clone)]
struct OssV4Transport {
    inner: ReqwestTransport,
    signer: Signer<reqsign_aliyun_oss::Credential>,
}

impl HttpTransport for OssV4Transport {
    async fn fetch(
        &self,
        request: http::Request<Buffer>,
    ) -> opendal::Result<http::Response<HttpBody>> {
        let (mut parts, body) = request.into_parts();
        // OpenDAL 0.58 的 OSS backend 默认先签 V1；发送前移除旧签名并使用明确地域改签 V4。
        parts.headers.remove(AUTHORIZATION);
        parts.headers.remove(DATE);
        parts.headers.remove("x-oss-date");
        parts.headers.remove("x-oss-content-sha256");
        parts.headers.remove("x-oss-additional-headers");
        self.signer.sign(&mut parts, None).await.map_err(|error| {
            Error::new(ErrorKind::Unexpected, "sign OSS request with V4")
                .with_operation("OssV4Transport::fetch")
                .set_source(error)
        })?;
        self.inner
            .fetch(http::Request::from_parts(parts, body))
            .await
    }
}

pub fn apply_transport(
    operator: opendal::Operator,
    account: &ObjectStorageAccountSnapshot,
    mount: &ObjectStorageMount,
) -> ObjectStorageResult<opendal::Operator> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| invalid("operator", "无法创建对象存储 HTTP transport"))?;
    let inner = ReqwestTransport::new(client);
    let transport = match account.provider {
        CloudProvider::TencentCos => HttpTransporter::new(inner),
        CloudProvider::AliyunOss => {
            let signer = Signer::new(
                Context::new(),
                StaticCredentialProvider::new(
                    account.access_key_id.expose_secret(),
                    account.access_key_secret.expose_secret(),
                ),
                RequestSigner::new(&mount.bucket)
                    .with_region(&mount.region)
                    .with_signing_version(SigningVersion::V4),
            );
            HttpTransporter::new(OssV4Transport { inner, signer })
        }
    };
    Ok(operator.with_context(OperationContext::new().with_http_transport(transport)))
}
