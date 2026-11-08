//! Planning rooms.
//!
//! The host's app runs a WebSocket server (TCP, `Settings::room_port`). When
//! it opens, it asks the router over UPnP for the public address and to
//! forward the port (renewed while open, removed on close); if the router
//! can't say, it asks api.ipify.org for the public address. The room code
//! holds the local and public addresses and the port; a guest tries both and
//! keeps the first that answers. The host relays each person's cursor,
//! pings, messages, view and selection (stamped with the real sender and
//! checked by `Wire::from_peer`) and keeps the
//! authoritative copy of the shared document: every edited document gets a
//! new revision and goes to everyone else. Messages are JSON
//! `fb_model::room::Wire` values. Each incoming message calls `notify` so the
//! UI can fetch it at once with `room_poll`.

use fb_model::room::{encode_code, is_shared_or_private, HostInfo, Peer, RoomAddress, RoomEvent, Wire, PROTOCOL_VERSION};
use futures_util::stream::FuturesUnordered;
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::task::AbortHandle;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

/// Biggest message accepted (a large factory document is a few hundred KB).
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const HOST_ID: u32 = 0;
const HOST_COLOR: [u8; 3] = [0xFA, 0x95, 0x49]; // the game's FICSIT primary preset
/// UPnP lease; renewed well before it runs out while the room is open.
const LEASE_SECS: u32 = 7200;
const PUBLIC_IP_SERVICE: &str = "api.ipify.org";

pub type Inbox = Arc<Mutex<VecDeque<RoomEvent>>>;
/// Called whenever something arrives for the UI.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct Events {
    queue: Inbox,
    notify: Notify,
}

impl Events {
    pub fn new(notify: Notify) -> Self {
        Self { queue: Default::default(), notify }
    }
    fn push(&self, e: RoomEvent) {
        self.queue.lock().unwrap().push_back(e);
        (self.notify)();
    }
    pub fn drain(&self) -> Vec<RoomEvent> {
        self.queue.lock().unwrap().drain(..).collect()
    }
    #[cfg(test)]
    pub fn queue(&self) -> &Inbox {
        &self.queue
    }
}

fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE))
}

/// Names show under cursors: keep them short and printable.
pub fn clean_name(name: &str) -> String {
    let n: String = name.chars().filter(|c| !c.is_control()).take(24).collect();
    let n = n.trim().to_string();
    if n.is_empty() {
        "Pioneer".into()
    } else {
        n
    }
}

/// A distinct, bright colour per guest (golden-ratio hue steps).
fn guest_color(id: u32) -> [u8; 3] {
    let h = (id as f32 * 0.618_034).fract() * 6.0;
    let (s, v) = (0.55_f32, 0.95_f32);
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8]
}

/// This PC's address on the local network. Connecting a UDP socket only picks
/// the outgoing interface; nothing is sent.
pub fn local_ipv4() -> Ipv4Addr {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .ok()
        .and_then(|a| match a.ip() {
            IpAddr::V4(v4) => Some(v4),
            _ => None,
        })
        .unwrap_or(Ipv4Addr::LOCALHOST)
}

// ------------------------------------------------------------ router + public

struct Reach {
    public: Option<Ipv4Addr>,
    source: Option<String>,
    forwarded: bool,
    problem: Option<String>,
    shared: bool,
    gateway: Option<igd_next::aio::Gateway<igd_next::aio::tokio::Tokio>>,
}

/// Asks the router for the public address and to forward `port`; falls back
/// to a public "what is my IP" service for the address.
async fn find_reach(local: Ipv4Addr, port: u16) -> Reach {
    let mut r = Reach { public: None, source: None, forwarded: false, problem: None, shared: false, gateway: None };
    let mut options = igd_next::SearchOptions::default();
    options.bind_addr = SocketAddr::new(IpAddr::V4(local), 0);
    options.timeout = Some(Duration::from_secs(3));
    options.single_search_timeout = Some(Duration::from_secs(2));
    match igd_next::aio::tokio::search_gateway(options).await {
        Ok(gw) => {
            match gw.get_external_ip().await {
                Ok(IpAddr::V4(ip)) if !is_shared_or_private(ip) => {
                    r.public = Some(ip);
                    r.source = Some("router".into());
                }
                Ok(IpAddr::V4(_)) => r.shared = true,
                _ => {}
            }
            let local_addr = SocketAddr::new(IpAddr::V4(local), port);
            match gw.add_port(igd_next::PortMappingProtocol::TCP, port, local_addr, LEASE_SECS, "Factory Builder room").await {
                Ok(()) => {
                    r.forwarded = true;
                    r.gateway = Some(gw);
                }
                Err(e) => r.problem = Some(format!("the router refused ({e})")),
            }
        }
        Err(_) => r.problem = Some("no router answered over UPnP (it may be turned off in the router's settings)".into()),
    }
    if r.public.is_none() {
        if let Some(ip) = public_ip_from_service().await {
            r.public = Some(ip);
            r.source = Some(PUBLIC_IP_SERVICE.into());
        }
    }
    r
}

/// One plain HTTP GET to api.ipify.org, which answers with the caller's IPv4
/// address as text. Anything else is ignored.
async fn public_ip_from_service() -> Option<Ipv4Addr> {
    let fetch = async {
        let mut s = TcpStream::connect((PUBLIC_IP_SERVICE, 80)).await.ok()?;
        let req = format!("GET / HTTP/1.1\r\nHost: {PUBLIC_IP_SERVICE}\r\nUser-Agent: FactoryBuilder\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await.ok()?;
        let mut buf = Vec::new();
        s.take(4096).read_to_end(&mut buf).await.ok()?;
        parse_ip_response(&buf)
    };
    tokio::time::timeout(Duration::from_secs(4), fetch).await.ok().flatten()
}

/// Body of an HTTP 200 response that is just a public IPv4 address.
fn parse_ip_response(raw: &[u8]) -> Option<Ipv4Addr> {
    let text = std::str::from_utf8(raw).ok()?;
    let (head, body) = text.split_once("\r\n\r\n")?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return None;
    }
    let ip: Ipv4Addr = body.trim().parse().ok()?;
    (!is_shared_or_private(ip)).then_some(ip)
}

// ---------------------------------------------------------------------- host

struct HostState {
    next_id: u32,
    peers: Vec<Peer>,
    senders: HashMap<u32, UnboundedSender<String>>,
    doc: Option<String>,
    rev: u64,
}

pub struct Host {
    state: Arc<Mutex<HostState>>,
    events: Events,
    tasks: Arc<Mutex<Vec<AbortHandle>>>,
    gateway: Option<igd_next::aio::Gateway<igd_next::aio::tokio::Tokio>>,
    runtime: tokio::runtime::Handle,
    pub info: HostInfo,
}

impl Host {
    /// Opens the room on all network interfaces, asking the router to
    /// forward the port.
    pub async fn open(name: &str, port: u16, notify: Notify) -> Result<Host, String> {
        Self::open_on(Ipv4Addr::UNSPECIFIED, name, port, true, notify).await
    }

    /// Opens the room on one address. Tests use loopback with `reach` off:
    /// no router, no outside service, no Windows Firewall prompt.
    pub async fn open_on(bind: Ipv4Addr, name: &str, port: u16, reach: bool, notify: Notify) -> Result<Host, String> {
        let listener = TcpListener::bind((bind, port))
            .await
            .map_err(|e| format!("Couldn't open TCP port {port}: {e}. Another program may be using it; pick another port in File › Settings."))?;
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
        let local = if bind.is_unspecified() { local_ipv4() } else { bind };
        let found = if reach {
            find_reach(local, port).await
        } else {
            Reach { public: None, source: None, forwarded: false, problem: None, shared: false, gateway: None }
        };
        let info = HostInfo {
            code: encode_code(RoomAddress { local, public: found.public, port }),
            local_ip: local.to_string(),
            port,
            public_ip: found.public.map(|p| p.to_string()),
            public_source: found.source.clone(),
            forwarded: found.forwarded,
            forward_problem: found.problem.clone(),
            shared_address: found.shared,
        };
        let state = Arc::new(Mutex::new(HostState {
            next_id: 1,
            peers: vec![Peer { id: HOST_ID, name: clean_name(name), color: HOST_COLOR }],
            senders: HashMap::new(),
            doc: None,
            rev: 0,
        }));
        let events = Events::new(notify);
        let tasks: Arc<Mutex<Vec<AbortHandle>>> = Default::default();
        let (st, ev, tk) = (state.clone(), events.clone(), tasks.clone());
        let accept = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (st, ev) = (st.clone(), ev.clone());
                let h = tokio::spawn(async move { serve_guest(stream, st, ev).await });
                tk.lock().unwrap().push(h.abort_handle());
            }
        });
        tasks.lock().unwrap().push(accept.abort_handle());
        if let Some(gw) = found.gateway.clone() {
            // Keep the forward alive while the room is open.
            let renew = tokio::spawn(async move {
                let addr = SocketAddr::new(IpAddr::V4(local), port);
                loop {
                    tokio::time::sleep(Duration::from_secs(LEASE_SECS as u64 / 2)).await;
                    let _ = gw.add_port(igd_next::PortMappingProtocol::TCP, port, addr, LEASE_SECS, "Factory Builder room").await;
                }
            });
            tasks.lock().unwrap().push(renew.abort_handle());
        }
        Ok(Host { state, events, tasks, gateway: found.gateway, runtime: tokio::runtime::Handle::current(), info })
    }

    pub fn events(&self) -> &Events {
        &self.events
    }

    /// Something the host's own UI did.
    pub fn send(&self, wire: Wire) {
        let mut st = self.state.lock().unwrap();
        match wire {
            Wire::Doc { json, .. } => {
                st.rev += 1;
                st.doc = Some(json.clone());
                let msg = Wire::Doc { from: HOST_ID, rev: st.rev, json };
                broadcast(&st, None, &msg);
            }
            // Cursor, ping, chat, view, selection: checked like a guest's.
            other => {
                if let Some(m) = other.from_peer(HOST_ID) {
                    broadcast(&st, None, &m);
                }
            }
        }
    }

    pub fn close(&mut self) {
        for t in self.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
        if let Some(gw) = self.gateway.take() {
            let port = self.info.port;
            self.runtime.spawn(async move {
                let _ = gw.remove_port(igd_next::PortMappingProtocol::TCP, port).await;
            });
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.close();
    }
}

fn broadcast(st: &HostState, except: Option<u32>, wire: &Wire) {
    let text = serde_json::to_string(wire).unwrap();
    for (id, tx) in &st.senders {
        if Some(*id) != except {
            let _ = tx.send(text.clone());
        }
    }
}

async fn serve_guest(stream: TcpStream, state: Arc<Mutex<HostState>>, events: Events) {
    let _ = stream.set_nodelay(true);
    let Ok(ws) = tokio_tungstenite::accept_async_with_config(stream, Some(ws_config())).await else { return };
    let (mut write, mut read) = ws.split();
    let (tx, mut rx) = unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if write.send(Message::text(text)).await.is_err() {
                break;
            }
        }
        let _ = write.close().await;
    });

    // First message must be Hello.
    let hello = tokio::time::timeout(Duration::from_secs(10), read.next()).await;
    let name = match hello {
        Ok(Some(Ok(Message::Text(t)))) => match serde_json::from_str::<Wire>(t.as_str()) {
            Ok(Wire::Hello { name, version }) if version == PROTOCOL_VERSION => clean_name(&name),
            Ok(Wire::Hello { .. }) => {
                let _ = tx.send(serde_json::to_string(&Wire::Denied { reason: "This room uses a different version of Factory Builder.".into() }).unwrap());
                drop(tx);
                let _ = writer.await;
                return;
            }
            _ => return,
        },
        _ => return,
    };

    let id = {
        let mut st = state.lock().unwrap();
        let id = st.next_id;
        st.next_id += 1;
        let peer = Peer { id, name, color: guest_color(id) };
        st.peers.push(peer.clone());
        let welcome = Wire::Welcome { you: id, peers: st.peers.clone(), doc: st.doc.clone(), rev: st.rev };
        let _ = tx.send(serde_json::to_string(&welcome).unwrap());
        broadcast(&st, None, &Wire::Joined { peer: peer.clone() });
        st.senders.insert(id, tx);
        events.push(RoomEvent::Message(Wire::Joined { peer }));
        id
    };

    while let Some(Ok(msg)) = read.next().await {
        let Message::Text(t) = msg else { continue };
        let Ok(wire) = serde_json::from_str::<Wire>(t.as_str()) else { continue };
        let mut st = state.lock().unwrap();
        match wire {
            Wire::Doc { json, .. } => {
                st.rev += 1;
                st.doc = Some(json.clone());
                let m = Wire::Doc { from: id, rev: st.rev, json };
                broadcast(&st, Some(id), &m);
                events.push(RoomEvent::Message(m));
            }
            // Cursor, ping, chat, view, selection: stamped with the real
            // sender, checked (finite numbers, sane zoom, cleaned text, a
            // capped node list) and passed to everyone else. Anything else a
            // guest sends (Hello again, Welcome, Joined...) is dropped.
            other => {
                if let Some(m) = other.from_peer(id) {
                    broadcast(&st, Some(id), &m);
                    events.push(RoomEvent::Message(m));
                }
            }
        }
    }

    let mut st = state.lock().unwrap();
    st.senders.remove(&id);
    st.peers.retain(|p| p.id != id);
    broadcast(&st, None, &Wire::Left { id });
    events.push(RoomEvent::Message(Wire::Left { id }));
    writer.abort();
}

// --------------------------------------------------------------------- guest

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// Tries every address at once; the first WebSocket that opens wins.
async fn connect_first(addrs: Vec<SocketAddrV4>) -> Result<(SocketAddrV4, Ws), String> {
    let mut tries: FuturesUnordered<_> = addrs
        .into_iter()
        .map(|addr| async move {
            let url = format!("ws://{addr}");
            let r = tokio::time::timeout(Duration::from_secs(6), tokio_tungstenite::connect_async_with_config(url, Some(ws_config()), true)).await;
            (addr, r)
        })
        .collect();
    let mut problems = Vec::new();
    while let Some((addr, r)) = tries.next().await {
        match r {
            Ok(Ok((ws, _))) => return Ok((addr, ws)),
            Ok(Err(e)) => problems.push(format!("{addr}: {e}")),
            Err(_) => problems.push(format!("{addr}: no answer")),
        }
    }
    Err(format!(
        "Couldn't reach the host ({}). Check the code. If you're not on the host's network, the host's port must be forwarded and allowed through their firewall.",
        problems.join("; ")
    ))
}

pub struct Guest {
    tx: UnboundedSender<String>,
    events: Events,
    tasks: Vec<AbortHandle>,
    pub connected_to: SocketAddrV4,
}

impl Guest {
    pub async fn join(addr: RoomAddress, name: &str, notify: Notify) -> Result<Guest, String> {
        let (connected_to, ws) = connect_first(addr.candidates()).await?;
        let (mut write, mut read) = ws.split();
        let events = Events::new(notify);
        let (tx, mut rx) = unbounded_channel::<String>();
        let hello = Wire::Hello { name: clean_name(name), version: PROTOCOL_VERSION };
        let _ = tx.send(serde_json::to_string(&hello).unwrap());
        let writer = tokio::spawn(async move {
            while let Some(text) = rx.recv().await {
                if write.send(Message::text(text)).await.is_err() {
                    break;
                }
            }
            let _ = write.close().await;
        });
        let ev = events.clone();
        let reader = tokio::spawn(async move {
            let mut reason = "The host closed the room.".to_string();
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(t)) => {
                        if let Ok(wire) = serde_json::from_str::<Wire>(t.as_str()) {
                            if let Wire::Denied { reason: r } = &wire {
                                reason = r.clone();
                            }
                            ev.push(RoomEvent::Message(wire));
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Ok(_) => {}
                    Err(e) => {
                        reason = format!("Lost the connection to the host ({e}).");
                        break;
                    }
                }
            }
            ev.push(RoomEvent::Closed(reason));
        });
        Ok(Guest { tx, events, tasks: vec![writer.abort_handle(), reader.abort_handle()], connected_to })
    }

    pub fn events(&self) -> &Events {
        &self.events
    }

    pub fn send(&self, wire: Wire) {
        let _ = self.tx.send(serde_json::to_string(&wire).unwrap());
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

pub enum Room {
    Host(Host),
    Guest(Guest),
}

impl Room {
    pub fn send(&self, wire: Wire) {
        match self {
            Room::Host(h) => h.send(wire),
            Room::Guest(g) => g.send(wire),
        }
    }
    pub fn drain(&self) -> Vec<RoomEvent> {
        match self {
            Room::Host(h) => h.events().drain(),
            Room::Guest(g) => g.events().drain(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet() -> Notify {
        Arc::new(|| {})
    }

    async fn wait_for(events: &Events, pred: impl Fn(&RoomEvent) -> bool) -> RoomEvent {
        for _ in 0..200 {
            let found = {
                let mut q = events.queue().lock().unwrap();
                q.iter().position(&pred).map(|i| q.remove(i).unwrap())
            };
            if let Some(e) = found {
                return e;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for an event; inbox: {:?}", events.queue().lock().unwrap());
    }

    fn code_for(port: u16) -> RoomAddress {
        RoomAddress { local: Ipv4Addr::LOCALHOST, public: None, port }
    }

    #[tokio::test]
    async fn guests_join_see_cursors_and_share_the_document() {
        let host = Host::open_on(Ipv4Addr::LOCALHOST, "Hoster", 0, false, quiet()).await.unwrap();
        let port = host.info.port;
        host.send(Wire::Doc { from: 0, rev: 0, json: "{\"v\":1}".into() });

        let a = Guest::join(code_for(port), "Ada", quiet()).await.unwrap();
        let welcome = wait_for(a.events(), |e| matches!(e, RoomEvent::Message(Wire::Welcome { .. }))).await;
        let RoomEvent::Message(Wire::Welcome { you, peers, doc, rev }) = welcome else { unreachable!() };
        assert_eq!(you, 1);
        assert_eq!(peers.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["Hoster", "Ada"]);
        assert_eq!(doc.as_deref(), Some("{\"v\":1}"), "late joiners get the current document");
        assert_eq!(rev, 1);
        wait_for(host.events(), |e| matches!(e, RoomEvent::Message(Wire::Joined { peer }) if peer.name == "Ada")).await;

        let b = Guest::join(code_for(port), "Bo", quiet()).await.unwrap();
        wait_for(b.events(), |e| matches!(e, RoomEvent::Message(Wire::Welcome { .. }))).await;
        wait_for(a.events(), |e| matches!(e, RoomEvent::Message(Wire::Joined { peer }) if peer.name == "Bo")).await;

        a.send(Wire::Cursor { id: 99, x: 10.0, y: 20.0, doc: 3 });
        wait_for(b.events(), |e| *e == RoomEvent::Message(Wire::Cursor { id: 1, x: 10.0, y: 20.0, doc: 3 })).await;
        wait_for(host.events(), |e| *e == RoomEvent::Message(Wire::Cursor { id: 1, x: 10.0, y: 20.0, doc: 3 })).await;

        b.send(Wire::Doc { from: 0, rev: 1, json: "{\"v\":2}".into() });
        wait_for(a.events(), |e| *e == RoomEvent::Message(Wire::Doc { from: 2, rev: 2, json: "{\"v\":2}".into() })).await;
        wait_for(host.events(), |e| matches!(e, RoomEvent::Message(Wire::Doc { rev: 2, .. }))).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!b.events().queue().lock().unwrap().iter().any(|e| matches!(e, RoomEvent::Message(Wire::Doc { .. }))), "no echo to the sender");

        host.send(Wire::Doc { from: 0, rev: 2, json: "{\"v\":3}".into() });
        wait_for(a.events(), |e| matches!(e, RoomEvent::Message(Wire::Doc { from: 0, rev: 3, .. }))).await;
        wait_for(b.events(), |e| matches!(e, RoomEvent::Message(Wire::Doc { from: 0, rev: 3, .. }))).await;

        drop(b);
        wait_for(a.events(), |e| *e == RoomEvent::Message(Wire::Left { id: 2 })).await;
        wait_for(host.events(), |e| *e == RoomEvent::Message(Wire::Left { id: 2 })).await;

        drop(host);
        wait_for(a.events(), |e| matches!(e, RoomEvent::Closed(_))).await;
    }

    /// Two guests and the host, everyone welcomed and known to each other.
    async fn room_of_three() -> (Host, Guest, Guest) {
        let host = Host::open_on(Ipv4Addr::LOCALHOST, "Hoster", 0, false, quiet()).await.unwrap();
        let a = Guest::join(code_for(host.info.port), "Ada", quiet()).await.unwrap();
        wait_for(a.events(), |e| matches!(e, RoomEvent::Message(Wire::Welcome { .. }))).await;
        let b = Guest::join(code_for(host.info.port), "Bo", quiet()).await.unwrap();
        wait_for(b.events(), |e| matches!(e, RoomEvent::Message(Wire::Welcome { .. }))).await;
        wait_for(a.events(), |e| matches!(e, RoomEvent::Message(Wire::Joined { peer }) if peer.name == "Bo")).await;
        wait_for(host.events(), |e| matches!(e, RoomEvent::Message(Wire::Joined { peer }) if peer.name == "Bo")).await;
        (host, a, b)
    }

    /// Nothing matching `pred` arrives at `events` (after letting the relay run).
    async fn never(events: &Events, pred: impl Fn(&RoomEvent) -> bool) {
        tokio::time::sleep(Duration::from_millis(80)).await;
        let q = events.queue().lock().unwrap();
        assert!(!q.iter().any(pred), "unexpected: {q:?}");
    }

    fn is_extra(e: &RoomEvent) -> bool {
        matches!(e, RoomEvent::Message(Wire::Ping { .. } | Wire::Chat { .. } | Wire::View { .. } | Wire::Selection { .. }))
    }

    #[tokio::test]
    async fn pings_messages_views_and_selections_are_relayed_with_the_real_sender() {
        use fb_model::room::NodeRef;
        let (host, a, b) = room_of_three().await;
        let nodes = vec![NodeRef { idx: 3, version: 1 }, NodeRef { idx: 7, version: 5 }];
        let sent = [
            Wire::Ping { id: 77, x: 10.0, y: -4.5, doc: 2 },
            Wire::Chat { id: 77, text: "over here".into() },
            Wire::View { id: 77, doc: 2, x: 300.0, y: 120.0, zoom: 0.5 },
            Wire::Selection { id: 77, doc: 2, nodes: nodes.clone() },
        ];
        for w in sent.clone() {
            a.send(w);
        }
        // Everyone else gets each one, with Ada's real id (1), not the one she claimed.
        for w in sent {
            let want = w.from_peer(1).unwrap();
            wait_for(b.events(), |e| *e == RoomEvent::Message(want.clone())).await;
            wait_for(host.events(), |e| *e == RoomEvent::Message(want.clone())).await;
        }
        never(a.events(), is_extra).await;

        // The host's own go to every guest as the host (id 0).
        host.send(Wire::Chat { id: 5, text: "hello all".into() });
        host.send(Wire::View { id: 5, doc: 0, x: 1.0, y: 2.0, zoom: 2.0 });
        for g in [&a, &b] {
            wait_for(g.events(), |e| *e == RoomEvent::Message(Wire::Chat { id: 0, text: "hello all".into() })).await;
            wait_for(g.events(), |e| *e == RoomEvent::Message(Wire::View { id: 0, doc: 0, x: 1.0, y: 2.0, zoom: 2.0 })).await;
        }
        never(host.events(), is_extra).await;
    }

    #[tokio::test]
    async fn bad_extras_are_dropped_and_messages_cleaned() {
        use fb_model::room::{NodeRef, MAX_CHAT, MAX_SELECTION};
        let (host, a, b) = room_of_three().await;
        // Malformed: none of these may reach anyone. (JSON has no NaN or
        // infinity, so a hostile peer's best tries are huge or out-of-range numbers.)
        for raw in [
            r#"{"t":"Ping","id":1,"x":1e39,"y":0,"doc":0}"#,
            r#"{"t":"Ping","id":1,"x":5e7,"y":0,"doc":0}"#,
            r#"{"t":"View","id":1,"doc":0,"x":0,"y":0,"zoom":0}"#,
            r#"{"t":"View","id":1,"doc":0,"x":0,"y":0,"zoom":1e30}"#,
            r#"{"t":"View","id":1,"doc":0,"x":0,"y":0,"zoom":-1}"#,
            r#"{"t":"Chat","id":1,"text":"\u0000\u0007\r\n  "}"#,
            r#"{"t":"Selection","id":1,"doc":0,"nodes":[{"idx":1}]}"#,
        ] {
            let _ = a.tx.send(raw.to_string());
        }
        a.send(Wire::Selection { id: 1, doc: 0, nodes: vec![NodeRef { idx: 1, version: 1 }; MAX_SELECTION + 1] });
        // A good one afterwards still gets through, so the link is fine.
        a.send(Wire::Ping { id: 1, x: 1.0, y: 1.0, doc: 0 });
        wait_for(b.events(), |e| matches!(e, RoomEvent::Message(Wire::Ping { x, .. }) if *x == 1.0)).await;
        wait_for(host.events(), |e| matches!(e, RoomEvent::Message(Wire::Ping { x, .. }) if *x == 1.0)).await;
        never(b.events(), is_extra).await;
        never(host.events(), is_extra).await;

        // The biggest selection allowed goes through.
        a.send(Wire::Selection { id: 1, doc: 0, nodes: vec![NodeRef { idx: 2, version: 1 }; MAX_SELECTION] });
        wait_for(b.events(), |e| matches!(e, RoomEvent::Message(Wire::Selection { nodes, .. }) if nodes.len() == MAX_SELECTION)).await;

        // Control and direction-override characters go; the rest is capped.
        let long = format!("\u{202E}hi\u{0007}\n{}", "x".repeat(400));
        a.send(Wire::Chat { id: 1, text: long });
        let RoomEvent::Message(Wire::Chat { text, .. }) = wait_for(b.events(), |e| matches!(e, RoomEvent::Message(Wire::Chat { .. }))).await else { unreachable!() };
        assert_eq!(text.chars().count(), MAX_CHAT);
        assert!(text.starts_with("hixxx"), "{text}");
        assert!(!text.chars().any(|c| c.is_control()));

        // The host's own messages are checked the same way.
        host.send(Wire::View { id: 0, doc: 0, x: 0.0, y: 0.0, zoom: 9.0 });
        host.send(Wire::Chat { id: 0, text: "\u{0008}".into() });
        never(a.events(), is_extra).await;
    }

    #[tokio::test]
    async fn guest_uses_whichever_address_answers() {
        // The "local" address in the code is dead (nothing on 127.0.0.2 at
        // this port); the "public" one is the host. The guest must still get in.
        let host = Host::open_on(Ipv4Addr::LOCALHOST, "Hoster", 0, false, quiet()).await.unwrap();
        let code = RoomAddress { local: Ipv4Addr::new(127, 0, 0, 2), public: Some(Ipv4Addr::LOCALHOST), port: host.info.port };
        let g = Guest::join(code, "Ada", quiet()).await.unwrap();
        assert_eq!(g.connected_to, SocketAddrV4::new(Ipv4Addr::LOCALHOST, host.info.port));
        wait_for(g.events(), |e| matches!(e, RoomEvent::Message(Wire::Welcome { .. }))).await;
    }

    #[tokio::test]
    async fn arrivals_notify_the_ui() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = count.clone();
        let host = Host::open_on(Ipv4Addr::LOCALHOST, "Hoster", 0, false, Arc::new(move || {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }))
        .await
        .unwrap();
        let g = Guest::join(code_for(host.info.port), "Ada", quiet()).await.unwrap();
        g.send(Wire::Cursor { id: 0, x: 1.0, y: 2.0, doc: 0 });
        wait_for(host.events(), |e| matches!(e, RoomEvent::Message(Wire::Cursor { .. }))).await;
        assert!(count.load(std::sync::atomic::Ordering::SeqCst) >= 2, "joined + cursor each notified");
    }

    #[tokio::test]
    async fn cursor_relay_latency_is_small() {
        let host = Host::open_on(Ipv4Addr::LOCALHOST, "Hoster", 0, false, quiet()).await.unwrap();
        let g = Guest::join(code_for(host.info.port), "Ada", quiet()).await.unwrap();
        wait_for(host.events(), |e| matches!(e, RoomEvent::Message(Wire::Joined { .. }))).await;
        let mut worst = Duration::ZERO;
        let mut total = Duration::ZERO;
        for i in 0..50 {
            let t = std::time::Instant::now();
            g.send(Wire::Cursor { id: 0, x: i as f32, y: 0.0, doc: 0 });
            loop {
                let got = host.events().drain().into_iter().any(|e| matches!(e, RoomEvent::Message(Wire::Cursor { x, .. }) if x == i as f32));
                if got {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let d = t.elapsed();
            worst = worst.max(d);
            total += d;
        }
        eprintln!("cursor relay over loopback: mean {:?}, worst {:?}", total / 50, worst);
        assert!(worst < Duration::from_millis(50), "worst {worst:?}");
    }

    #[tokio::test]
    async fn joining_nowhere_fails_with_a_helpful_message() {
        let err = Guest::join(code_for(1), "Ada", quiet()).await.err().expect("should fail");
        assert!(err.contains("127.0.0.1:1"), "{err}");
    }

    #[test]
    fn public_ip_responses_are_parsed_strictly() {
        assert_eq!(parse_ip_response(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n86.12.200.7"), Some(Ipv4Addr::new(86, 12, 200, 7)));
        assert_eq!(parse_ip_response(b"HTTP/1.1 500 Oops\r\n\r\n86.12.200.7"), None);
        assert_eq!(parse_ip_response(b"HTTP/1.1 200 OK\r\n\r\n<html>"), None);
        assert_eq!(parse_ip_response(b"HTTP/1.1 200 OK\r\n\r\n192.168.0.4"), None, "a private address can't be a public one");
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  "), "Pioneer");
        assert_eq!(clean_name(&"x".repeat(40)).len(), 24);
    }
}
