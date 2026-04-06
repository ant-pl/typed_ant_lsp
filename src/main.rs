use std::{collections::HashMap, sync::Arc};

use lsp_backend::Backend;
use tokio::sync::RwLock;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) =
        LspService::new(|client| Backend(client, Arc::new(RwLock::new(HashMap::new()))));

    Server::new(stdin, stdout, socket).serve(service).await;
}
