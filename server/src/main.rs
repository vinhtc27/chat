use std::sync::Arc;

use anyhow::Context;
use room_manager::RoomManagerBuilder;
use tokio::{net::TcpListener, signal::ctrl_c, sync::broadcast, task::JoinSet};

use crate::room_manager::ChatRoomMetadata;

mod room_manager;
mod session;

const DEFAULT_TCP_SERVER_ADDR: &str = "127.0.0.1:8080";
const DEFAULT_WS_SERVER_ADDR: &str = "127.0.0.1:8081";
const CHAT_ROOMS_METADATAS: &str = include_str!("../resources/chat_rooms_metadatas.json");

#[tokio::main]
async fn main() {
    let chat_room_metadatas: Vec<ChatRoomMetadata> = serde_json::from_str(CHAT_ROOMS_METADATAS)
        .expect("could not parse the chat rooms metadatas");
    let room_manager = Arc::new(
        chat_room_metadatas
            .into_iter()
            .fold(RoomManagerBuilder::new(), |builder, metadata| {
                builder.create_room(metadata)
            })
            .build(),
    );

    let mut join_set: JoinSet<anyhow::Result<()>> = JoinSet::new();
    let (quit_tx, quit_rx) = broadcast::channel::<()>(1);

    let tcp_server = TcpListener::bind(DEFAULT_TCP_SERVER_ADDR)
        .await
        .expect("could not bind to the port");

    let websocket_server = TcpListener::bind(DEFAULT_WS_SERVER_ADDR)
        .await
        .expect("could not bind to the port");
    println!("Tcp on {DEFAULT_TCP_SERVER_ADDR} | Ws on - {DEFAULT_WS_SERVER_ADDR}");
    loop {
        tokio::select! {
            Ok(_) = ctrl_c() => {
                println!("Server interrupted. Gracefully shutting down.");
                quit_tx.send(()).context("failed to send quit signal").unwrap();
                break;
            }
            Ok((socket, _)) = tcp_server.accept() => {
                join_set.spawn(session::handle_user_tcp_session(Arc::clone(&room_manager), quit_rx.resubscribe(), socket));
            }
            Ok((socket, _)) = websocket_server.accept() => {
                join_set.spawn(session::handle_user_ws_session(Arc::clone(&room_manager), quit_rx.resubscribe(), socket));
            }
        }
    }

    while join_set.join_next().await.is_some() {}
    println!("Server shut down");
}
