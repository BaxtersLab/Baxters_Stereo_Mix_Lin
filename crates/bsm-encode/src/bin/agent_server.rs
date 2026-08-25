use bsm_encode::ipc;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // listen on localhost:4000 by default
    ipc::run_agent_server("127.0.0.1:4000").await
}
