use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    ops::Deref,
    sync::{
        atomic::{AtomicU64, Ordering},
        LazyLock,
    },
};

use axum::{
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use axum_extra::{
    headers::{authorization::Bearer, Authorization},
    TypedHeader,
};
use chashmap::CHashMap;
use lateinit::LateInit;
use quinn::{Connection as QuinnConnection, VarInt};
use tracing::warn;
use uuid::Uuid;

use crate::CONFIG;

static ONLINE_COUNTER: LateInit<HashMap<Uuid, AtomicU64>> = LateInit::new();
static ONLINE_CLIENTS: LazyLock<CHashMap<Uuid, HashSet<QuicClient>>> = LazyLock::new(CHashMap::new);

#[derive(Clone)]
struct QuicClient(QuinnConnection);
impl Deref for QuicClient {
    type Target = QuinnConnection;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl From<QuinnConnection> for QuicClient {
    fn from(value: QuinnConnection) -> Self {
        Self(value)
    }
}
impl std::hash::Hash for QuicClient {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.stable_id().hash(state);
    }
}
impl PartialEq for QuicClient {
    fn eq(&self, other: &Self) -> bool {
        self.0.stable_id() == other.0.stable_id()
    }
}
impl Eq for QuicClient {}

pub async fn start() {
    let mut online = HashMap::new();
    for (user, _) in CONFIG.users.iter() {
        online.insert(user.to_owned(), AtomicU64::new(0));
    }
    unsafe { ONLINE_COUNTER.init(online) };

    let restful = CONFIG.restful.as_ref().unwrap();
    let addr = restful.addr;
    let app = Router::new()
        .route("/kick", post(kick))
        .route("/online", get(list_online))
        .route("/detailed_online", get(list_detailed_online));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    warn!("RESTful server started, listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}

async fn kick(
    TypedHeader(token): TypedHeader<Authorization<Bearer>>,
    Json(users): Json<Vec<Uuid>>,
) -> StatusCode {
    if let Some(restful) = &CONFIG.restful
        && restful.secret != token.token()
    {
        return StatusCode::UNAUTHORIZED;
    }
    for user in users {
        if let Some(list) = ONLINE_CLIENTS.get(&user).await {
            for client in list.iter() {
                client.close(VarInt::from_u32(6002), "Client got kicked".as_bytes());
            }
        }
    }
    StatusCode::OK
}

async fn list_online(
    TypedHeader(token): TypedHeader<Authorization<Bearer>>,
) -> (StatusCode, Json<HashMap<Uuid, u64>>) {
    if let Some(restful) = &CONFIG.restful
        && restful.secret != token.token()
    {
        return (StatusCode::UNAUTHORIZED, Json(HashMap::new()));
    }
    let mut result = HashMap::new();
    for (user, count) in ONLINE_COUNTER.iter() {
        let count = count.load(Ordering::Relaxed);
        if count != 0 {
            result.insert(user.to_owned(), count);
        }
    }

    (StatusCode::OK, Json(result))
}

async fn list_detailed_online(
    TypedHeader(token): TypedHeader<Authorization<Bearer>>,
) -> (StatusCode, Json<HashMap<Uuid, Vec<SocketAddr>>>) {
    if let Some(restful) = &CONFIG.restful
        && restful.secret != token.token()
    {
        return (StatusCode::UNAUTHORIZED, Json(HashMap::new()));
    }
    let mut result = HashMap::new();
    for (user, list) in ONLINE_CLIENTS.clone_locking().await.into_iter() {
        if list.is_empty() {
            continue;
        }
        result.insert(user, list.into_iter().map(|v| v.remote_address()).collect());
    }

    (StatusCode::OK, Json(result))
}

pub async fn client_connect(uuid: &Uuid, conn: QuinnConnection) {
    if CONFIG.restful.is_none() {
        return;
    }
    let cfg = CONFIG.restful.as_ref().unwrap();
    let current = ONLINE_COUNTER
        .get(uuid)
        .expect("Authorized UUID not present in users table")
        .fetch_add(1, Ordering::Release);
    if cfg.maximum_clients_per_user != 0 && current > cfg.maximum_clients_per_user {
        conn.close(
            VarInt::from_u32(6001),
            "Reached maximum clients limitation".as_bytes(),
        );
        return;
    }
    ONLINE_CLIENTS
        .upsert(*uuid, HashSet::new, |v| {
            v.insert(conn.into());
        })
        .await;
}
pub async fn client_disconnect(uuid: &Uuid, conn: QuinnConnection) {
    if CONFIG.restful.is_none() {
        return;
    }
    ONLINE_COUNTER
        .get(uuid)
        .expect("Authorized UUID not present in users table")
        .fetch_sub(1, Ordering::Release);
    if let Some(mut pair) = ONLINE_CLIENTS.get_mut(uuid).await {
        pair.remove(&conn.into());
    }
}
