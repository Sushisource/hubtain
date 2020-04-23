use crate::models::ClientId;
use anyhow::Error;
use async_std::io;
use async_trait::async_trait;
use futures::lock::Mutex;

lazy_static! {
    pub static ref CONSOLE_APPROVER: ConsoleApprover = ConsoleApprover::default();
}

#[async_trait]
pub trait ClientApprover: Sync + Send {
    /// Submit a client for approval. Resolves when the client is approved or rejected, true for
    /// approved.
    async fn submit(&self, client_id: ClientId) -> Result<bool, Error>;
}

/// Interactive console based approval. Approval is necessarily serialized.
#[derive(Default)]
pub struct ConsoleApprover {
    lock: Mutex<()>,
}

#[async_trait]
impl ClientApprover for ConsoleApprover {
    async fn submit(&self, client_id: ClientId) -> Result<bool, Error> {
        let _drop_on_return = self.lock.lock().await;
        println!("Approve '{}'?", client_id);
        let mut input = String::new();
        io::stdin().read_line(&mut input).await?;
        let trimmed = input.trim();
        if trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes") {
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
