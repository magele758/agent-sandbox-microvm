use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    let token = std::env::var("ENVD_TOKEN").unwrap_or_default();
    let root = std::env::var("WORKSPACE_ROOT").unwrap_or_else(|_| "/workspace".to_string());
    let app = guest_agent::router_with_root(token, std::path::PathBuf::from(root));
    let addr: SocketAddr = std::env::var("ADDR")
        .unwrap_or_else(|_| "0.0.0.0:49983".to_string())
        .parse()
        .expect("listen address");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind guest address");
    axum::serve(listener, app).await.expect("serve");
}
