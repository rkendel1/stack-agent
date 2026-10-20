use feltdb::{
    authority::{AuthorityClient, AuthorityTransaction, FeltDbAuthority},
    AtomicMutation,
};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let socket = arguments
        .next()
        .ok_or("usage: feltdb-authority-client <socket> <id>")?;
    let id = arguments
        .next()
        .ok_or("usage: feltdb-authority-client <socket> <id>")?;
    let client = AuthorityClient::connect(socket);
    client
        .commit(AuthorityTransaction {
            transaction_id: format!("writer-{id}"),
            base_revision: None,
            preconditions: vec![],
            record_preconditions: vec![],
            mutations: vec![AtomicMutation {
                capability: "registrations".into(),
                key: format!("registrations:{id}"),
                value: Some(json!({ "id": id })),
            }],
            audit: None,
        })
        .await?;
    Ok(())
}
