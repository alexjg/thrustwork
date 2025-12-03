use automerge::transaction::Transactable;
use automerge::{Automerge, ReadDoc};
use samod::{ConnDirection, DocumentId};
use std::env;
use std::str::FromStr;
use tokio_tungstenite::connect_async;

mod test_autosurgeon;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    // Initialize a samod Repo with in-memory storage
    let repo = samod::Repo::build_tokio().load().await;

    println!("Repo initialized successfully");
    println!("Repo peer ID: {}", repo.peer_id());

    // Connect to the Automerge sync server
    let sync_server_url = "wss://sync3.automerge.org";
    println!("Connecting to sync server: {}", sync_server_url);

    let (ws_stream, _response) = match connect_async(sync_server_url).await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Failed to connect to sync server: {}", e);
            std::process::exit(1);
        }
    };

    println!("WebSocket connected, starting sync protocol...");

    // Spawn the connection handler - it runs in the background
    let conn_future = repo.connect_tungstenite(ws_stream, ConnDirection::Outgoing);
    tokio::spawn(conn_future);

    println!("Connected to sync server");

    if args.len() > 1 {
        // Read existing document by URL
        let url_str = &args[1];
        println!("Looking up document: {}", url_str);

        // Give time for sync protocol to establish
        println!("Waiting for sync protocol to establish...");
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        // Parse the document ID from the URL (format: automerge:<doc_id>)
        let doc_id_str = url_str
            .strip_prefix("automerge:")
            .expect("URL must start with 'automerge:'");

        let doc_id = DocumentId::from_str(doc_id_str).expect("Invalid document ID");

        let doc_handle = repo
            .find(doc_id)
            .await
            .expect("Repo stopped")
            .expect("Document not found");

        println!("Found document!");

        // Read the document contents
        doc_handle.with_document(|doc| {
            // Get all keys at the root
            let keys: Vec<_> = doc.keys(automerge::ROOT).collect();
            println!("Document keys: {:?}", keys);

            // Try to read the "test" key
            if let Some((value, _)) = doc.get(automerge::ROOT, "test").unwrap() {
                println!("Document contents: test = {:?}", value);
            }
        });
    } else {
        // Create a new document with a test value
        let mut doc = Automerge::new();
        let _change = doc
            .transact::<_, _, automerge::AutomergeError>(|txn| {
                txn.put(automerge::ROOT, "test", "hello from thrustwork")?;
                Ok(())
            })
            .expect("Failed to create initial document content");

        let doc_handle = repo.create(doc).await.expect("Failed to create document");

        println!("Created document: {}", doc_handle.url());

        // Wait a bit for sync to complete
        println!("Waiting for sync...");
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }

    println!("Done!");
}
