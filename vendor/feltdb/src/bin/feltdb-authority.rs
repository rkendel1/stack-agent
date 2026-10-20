use feltdb::authority::AuthorityServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let socket = arguments
        .next()
        .ok_or("usage: feltdb-authority <socket> <storage>")?;
    let storage = arguments
        .next()
        .ok_or("usage: feltdb-authority <socket> <storage>")?;
    if arguments.next().is_some() {
        return Err("usage: feltdb-authority <socket> <storage>".into());
    }
    AuthorityServer::open(socket, std::path::PathBuf::from(storage))?
        .serve()
        .await?;
    Ok(())
}
