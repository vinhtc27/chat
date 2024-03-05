use std::sync::Arc;

use comms::{
    command::UserCommand,
    event::{self, RoomDetail},
    transport,
};
use futures_util::{stream::StreamExt, SinkExt};
use nanoid::nanoid;
use tokio::{net::TcpStream, sync::broadcast};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::room_manager::RoomManager;

use self::chat_session::ChatSession;

mod chat_session;

/// Given a tcp stream and a room manager, handles the user session
/// until the user quits the session, or the tcp stream is closed for some reason, or the server shuts down
#[allow(dead_code)]
pub async fn handle_user_tcp_session(
    room_manager: Arc<RoomManager>,
    mut quit_rx: broadcast::Receiver<()>,
    stream: TcpStream,
) -> anyhow::Result<()> {
    let session_id = nanoid!();
    // Generate a random id for the user, since we don't have a login system
    let user_id = String::from(&nanoid!()[0..5]);
    // Split the tcp stream into a command stream and an event writer with better ergonomics
    let (mut commands, mut event_writer) = transport::server::split_tcp_stream(stream);

    // Welcoming the user with a login successful event and necessary information about the server
    event_writer
        .write(&event::Event::LoginSuccessful(
            event::LoginSuccessfulReplyEvent {
                session_id: session_id.clone(),
                user_id: user_id.clone(),
                rooms: room_manager
                    .chat_room_metadatas()
                    .iter()
                    .map(|metadata| RoomDetail {
                        name: metadata.name.clone(),
                        description: metadata.description.clone(),
                    })
                    .collect(),
            },
        ))
        .await?;

    // Create a chat session with the given room manager
    // Chat Session will abstract the user session handling logic for multiple rooms
    let mut chat_session = ChatSession::new(&session_id, &user_id, room_manager);

    loop {
        tokio::select! {
            cmd = commands.next() => match cmd {
                // If the user closes the tcp stream, or sends a quit cmd
                // We need to cleanup resources in a way that the other users are notified about the user's departure
                None | Some(Ok(UserCommand::Quit(_))) => {
                    chat_session.leave_all_rooms().await?;
                    break;
                }
                // Handle a valid user command
                Some(Ok(cmd)) => match cmd {
                    // For user session related commands, we need to handle them in the chat session
                    UserCommand::JoinRoom(_) | UserCommand::SendMessage(_) | UserCommand::LeaveRoom(_) => {
                        chat_session.handle_user_command(cmd).await?;
                    }
                    _ => {}
                }
                _ => {}
            },
            // Aggregated events from the chat session are sent to the user
            Ok(chat_event) = chat_session.recv() => {
                event_writer.write(&chat_event).await?;
            }
            // If the server is shutting down, we can just close the tcp streams
            // and exit the session handler. Since the server is shutting down,
            // we don't need to notify other users about the user's departure or cleanup resources
            Ok(_) = quit_rx.recv() => {
                drop(event_writer);
                println!("Gracefully shutting down user tcp stream.");
                break;
            }
        }
    }

    Ok(())
}

pub async fn handle_user_ws_session(
    room_manager: Arc<RoomManager>,
    mut quit_rx: broadcast::Receiver<()>,
    stream: TcpStream,
) -> anyhow::Result<()> {
    let session_id = nanoid!();
    // Generate a random id for the user, since we don't have a login system
    let user_id = String::from(&nanoid!()[0..5]);

    // Create a websocket writer and reader from the tcp stream
    let (mut ws_writer, mut ws_reader) = accept_async(stream).await?.split();

    // Welcoming the user with a login successful event and necessary information about the server
    let login_event = event::Event::LoginSuccessful(event::LoginSuccessfulReplyEvent {
        session_id: session_id.clone(),
        user_id: user_id.clone(),
        rooms: room_manager
            .chat_room_metadatas()
            .iter()
            .map(|metadata| RoomDetail {
                name: metadata.name.clone(),
                description: metadata.description.clone(),
            })
            .collect(),
    });

    ws_writer
        .send(Message::Text(serde_json::to_string(&login_event).unwrap()))
        .await?;

    // Create a chat session with the given room manager
    // Chat Session will abstract the user session handling logic for multiple rooms
    let mut chat_session = ChatSession::new(&session_id, &user_id, room_manager);

    loop {
        tokio::select! {
            // Handle websocket events
            message = ws_reader.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    let cmd: UserCommand = serde_json::from_str(&text).unwrap();
                    match cmd {
                        UserCommand::Quit(_) => {
                            chat_session.leave_all_rooms().await?;
                            break;
                        }
                        UserCommand::JoinRoom(_) | UserCommand::SendMessage(_) | UserCommand::LeaveRoom(_) => {
                            chat_session.handle_user_command(cmd).await?;
                        }
                    }
                }
                Some(Ok(Message::Ping(_))) => {
                    ws_writer.send(Message::Pong(vec![])).await?;
                }
                Some(Ok(Message::Close(_))) | Some(Err(_))=> break,
                None | Some(Ok(Message::Pong(_))) | Some(Ok(Message::Binary(_))) |Some(Ok(Message::Frame(_)))=> unreachable!(),
            },
            // Aggregated events from the chat session are sent to the user
            Ok(chat_event) = chat_session.recv() => {
                ws_writer.send(Message::Text(serde_json::to_string(&chat_event).unwrap())).await?;
            }
            // If the server is shutting down, we can just close the tcp streams
            // and exit the session handler. Since the server is shutting down,
            // we don't need to notify other users about the user's departure or cleanup resources
            Ok(_) = quit_rx.recv() => {
                println!("Gracefully shutting down user websocket stream.");
                break;
            }
        }
    }
    Ok(())
}
