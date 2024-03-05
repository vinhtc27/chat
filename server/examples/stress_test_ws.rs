use std::time::Duration;

use comms::{
    command::{JoinRoomCommand, UserCommand},
    event::Event,
};
use futures_util::{SinkExt, StreamExt};
use nanoid::nanoid;
use rand::{rngs::StdRng, Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Stres Test for the Chat Server
///
/// Generates synthetic load with users who joins and sends messages to random roms.
/// The number of users, number of rooms joined per user and chattines of users can be configured.
///
/// !IMPORTANT! Be sure to check and configure your socket limits, before you run the tests

const DEFAULT_WS_SERVER_ADDR: &str = "127.0.0.1:8081";
const CHAT_ROOMS_METADATAS: &str = include_str!("../resources/chat_rooms_metadatas.json");

/// Stress Test Configuration
// The number of users to spawn, distributed across the load increments
const LOAD_INCREMENTS: &str = r#"[
    { "user_count": 1200, "after": { "secs": 60, "nanos": 0 }, "steps": 60 },
    { "user_count": 2400, "after": { "secs": 120, "nanos": 0 }, "steps": 60 }
]"#;
// How many rooms a user should join, this affects the total tokio task count
const NUMBER_OF_ROOMS_TO_JOIN: usize = 5;
// How many milliseconds to wait between each user message
const USER_CHAT_DELAY_MILLIS: u64 = 1_000;

/// [RotatingIterator] is a simple iterator that rotates through a list of items
/// and starts from the beginning when the end is reached.
struct RotatingIterator<T> {
    items: Vec<T>,
    current: usize,
}

impl<T> RotatingIterator<T> {
    fn new(items: Vec<T>) -> Self {
        Self { items, current: 0 }
    }
}

impl<T: Clone> Iterator for RotatingIterator<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.items.get(self.current).cloned();
        self.current = (self.current + 1) % self.items.len();
        item
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatRoomMetadata {
    name: String,
    description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoadIncrements {
    user_count: usize,
    after: Duration,
    steps: usize,
}

async fn spawn_single_user(rooms_to_join: Vec<String>) -> anyhow::Result<()> {
    let result = spawn_single_user_raw(rooms_to_join).await;

    match result.as_ref() {
        Ok(_) => println!("exited without problems"),
        Err(err) => println!("some error occurred = {}", err.to_string()),
    }

    result
}

async fn spawn_single_user_raw(rooms_to_join: Vec<String>) -> anyhow::Result<()> {
    let url = url::Url::parse(format! {"ws://{DEFAULT_WS_SERVER_ADDR}"}.as_str()).unwrap();
    let (websocket, _) = connect_async(url).await?;
    let (mut ws_writer, mut ws_reader) = websocket.split();

    let _ = match ws_reader.next().await {
        Some(Ok(Message::Text(text))) => {
            let event: Event = serde_json::from_str(&text).unwrap();
            match event {
                Event::LoginSuccessful(login_event) => login_event,
                _ => return Err(anyhow::anyhow!("server did not send login successfull")),
            }
        }
        _ => return Err(anyhow::anyhow!("server did not send message successfull")),
    };

    for room_name in rooms_to_join.iter() {
        let command = UserCommand::JoinRoom(JoinRoomCommand {
            room: String::from(room_name),
        });
        ws_writer
            .send(Message::Text(serde_json::to_string(&command).unwrap()))
            .await?;
    }

    let join_handle = tokio::spawn({
        let mut rng = StdRng::from_entropy();
        let mut rooms_iterator = RotatingIterator::new(rooms_to_join);
        let to_sleep = Duration::from_millis(USER_CHAT_DELAY_MILLIS);

        async move {
            // sleep initially for a time to distribute the messaging times
            tokio::time::sleep(Duration::from_millis(
                rng.gen_range(1..USER_CHAT_DELAY_MILLIS),
            ))
            .await;

            loop {
                let room_name = rooms_iterator.next().unwrap();
                let command = UserCommand::SendMessage(comms::command::SendMessageCommand {
                    room: String::from(room_name),
                    content: nanoid!(),
                });
                let _ = ws_writer
                    .send(Message::Text(serde_json::to_string(&command).unwrap()))
                    .await;

                tokio::time::sleep(to_sleep).await;
            }
        }
    });

    while let Some(_) = ws_reader.next().await {}

    join_handle.abort();
    Ok(())
}

#[tokio::main]
async fn main() {
    let load_increments: Vec<LoadIncrements> =
        serde_json::from_str(LOAD_INCREMENTS).expect("could not parse the load increments");
    let chat_room_metadatas: Vec<ChatRoomMetadata> = serde_json::from_str(CHAT_ROOMS_METADATAS)
        .expect("could not parse the chat rooms metadatas");

    let mut room_iterator = RotatingIterator::new(chat_room_metadatas);
    let mut join_set: JoinSet<anyhow::Result<()>> = JoinSet::new();

    let mut current: usize = 0;
    for li in load_increments {
        let diff = li.user_count - current;
        let sleep_duration =
            Duration::from_millis((li.after.as_millis() / li.steps as u128) as u64);
        let to_increment = diff / li.steps;

        for _ in 0..li.steps {
            for _ in 0..to_increment {
                let rooms_to_join = room_iterator
                    .by_ref()
                    .take(NUMBER_OF_ROOMS_TO_JOIN)
                    .map(|metadata| metadata.name.clone())
                    .collect();

                join_set.spawn(spawn_single_user(rooms_to_join));
            }

            current += to_increment;
            println!("total users: {}", current);
            tokio::time::sleep(sleep_duration).await;
        }
    }

    while let Some(_) = join_set.join_next().await {}
}
