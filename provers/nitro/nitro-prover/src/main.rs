use nitro_prover::{protocol_helper::*, NitroProver, PORT};
use raiko_lib::{input::GuestInput, prover::Prover};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use vsock::{VsockAddr, VsockListener};

#[tokio::main]
async fn main() {
    // start tracing + logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    if let Err(e) = tracing::subscriber::set_global_default(subscriber) {
        println!(
            "Failed to set_global_default of tracing subscriber with details {}",
            e
        );
    }
    info!("Initializing");
    let listener = VsockListener::bind(&VsockAddr::new(libc::VMADDR_CID_ANY, PORT))
        .expect("bind and listen failed");
    info!("Listener socket binded. Starting main loop");
    for stream in listener.incoming() {
        info!("Received new proof request");
        match stream {
            Ok(mut v_stream) => {
                info!("Reading message from the socket");
                let Ok(data) = recv_message(&mut v_stream) else {
                    info!("Failed to read whole GuestInput bytes from socket!");
                    continue;
                };
                let Ok(guest_input) = serde_json::from_slice::<GuestInput>(&data) else {
                    info!("Provided bytes are not json serialized GuestInput");
                    continue;
                };
                let block = guest_input.block_number;
                match NitroProver::run(guest_input, &Default::default(), &Default::default()).await
                {
                    Err(e) => {
                        info!(
                            "Failed to generate nitro proof for block {} with details {}",
                            block, e
                        );
                        continue;
                    }
                    Ok(proof_value) => {
                        let Ok(proof) = serde_json::to_string(&proof_value) else {
                            info!("Proof type unexpected for block {}!", block);
                            continue;
                        };
                        let Ok(_) = send_message(&mut v_stream, proof) else {
                            info!("Failed to write proof back into socket. Client disconnected?");
                            continue;
                        };
                    }
                }
            }

            Err(err) => info!("Accept failed: {:?}", err),
        }
    }
}
