use node_agent::router;

#[tokio::main]
async fn main() {
    let addr = std::env::var("ADDR").unwrap_or_else(|_| "0.0.0.0:5008".to_string());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("bind node address");
    axum::serve(listener, router()).await.expect("serve");
}
