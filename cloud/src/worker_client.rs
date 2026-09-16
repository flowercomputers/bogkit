use crate::CloudError;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

#[derive(Clone)]
pub struct WorkerClient {
    client: reqwest::Client,
}
impl WorkerClient {
    pub fn new(socket: &Path) -> Result<Self, CloudError> {
        let client = reqwest::Client::builder()
            .unix_socket(socket)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .pool_max_idle_per_host(4)
            .build()
            .map_err(|_| CloudError::new("unavailable", "worker client unavailable"))?;
        Ok(Self { client })
    }
    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value), CloudError> {
        if !path.starts_with('/') || path.starts_with("//") || path.contains('#') {
            return Err(CloudError::new(
                "invalid_request",
                "invalid worker operation",
            ));
        }
        let mut request = self.client.request(method, format!("http://worker{path}"));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| CloudError::new("unavailable", "database worker unavailable"))?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| CloudError::new("unavailable", "worker response interrupted"))?
        {
            if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                return Err(CloudError::new(
                    "response_too_large",
                    "reduce the requested page size",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = serde_json::from_slice(&bytes)
            .map_err(|_| CloudError::new("unavailable", "invalid worker response"))?;
        Ok((status, value))
    }
}
