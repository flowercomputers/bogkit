#![allow(dead_code)]
use bog_cloud::{CloudService, config::Config};
use rmcp::{
    RoleClient, ServiceExt,
    service::RunningService,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use std::{path::PathBuf, sync::Arc};
pub const OWNER: &str = "mcp-test-owner-token-32-characters-long";
pub struct Harness {
    pub service: Arc<CloudService>,
    pub url: String,
    task: tokio::task::JoinHandle<()>,
    _root: tempfile::TempDir,
}
impl Harness {
    pub async fn new() -> Self {
        Self::with_options(bog_cloud_mcp::McpOptions::default()).await
    }
    pub async fn with_options(options: bog_cloud_mcp::McpOptions) -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target/debug/bog-records-worker")
            .canonicalize()
            .expect("build bog-records-worker first");
        let service = CloudService::open(Config::new(root.path().into(), worker), OWNER).unwrap();
        let app = bog_cloud::build_rest_router(service.clone()).merge(
            bog_cloud_mcp::build_mcp_router_with_options(service.clone(), options),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            service,
            url,
            task,
            _root: root,
        }
    }
    pub async fn client(&self, token: &str) -> RunningService<RoleClient, ()> {
        let config = StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", self.url))
            .auth_header(token);
        ().serve(StreamableHttpClientTransport::with_client(
            reqwest::Client::new(),
            config,
        ))
        .await
        .expect("SDK initialization")
    }
    pub async fn close(self) {
        self.service.supervisor.shutdown().await.unwrap();
        self.task.abort();
    }
}
pub async fn call(
    client: &RunningService<RoleClient, ()>,
    name: &str,
    args: serde_json::Value,
) -> rmcp::model::CallToolResult {
    client
        .call_tool(
            rmcp::model::CallToolRequestParams::new(name.to_owned())
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap()
}
pub async fn ready(h: &Harness, id: bog_cloud::BogId) {
    h.service.supervisor.ensure_running(id).await.unwrap();
}
