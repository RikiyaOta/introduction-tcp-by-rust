use crate::packet::TCPPacket;
use crate::socket::{SockID, Socket, TcpStatus};
use crate::tcpflags;
use anyhow::{Context, Result};
use pnet::packet::{ip::IpNextHeaderProtocols, tcp::TcpPacket, Packet};
use pnet::transport::{self, TransportChannelType};
use rand::{rngs::ThreadRng, Rng};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, RwLock, RwLockWriteGuard};
use std::time::{Duration, SystemTime};
use std::{cmp, ops::Range, str, thread};

const UNDETERMINED_IP_ADDR: std::net::Ipv4Addr = Ipv4Addr::new(0, 0, 0, 0);
const UNDETERMINED_PORT: u16 = 0;
const MAX_TRANSMITTION: u8 = 5;
const RETRANSMITTION_TIMEOUT: u64 = 3;
const MSS: usize = 1460;
const PORT_RANGE: Range<u16> = 40000..60000;

pub struct TCP {
    // TCP 全体の管理を3つのスレッドから扱うため。
    sockets: RwLock<HashMap<SockID, Socket>>,
    event_condvar: (Mutex<Option<TCPEvent>>, Condvar),
}

impl TCP {
    pub fn new() -> Arc<Self> {
        let sockets = RwLock::new(HashMap::new());
        let tcp = Arc::new(Self {
            sockets,
            event_condvar: (Mutex::new(None), Condvar::new()),
        });
        let cloned_tcp = tcp.clone();
        std::thread::spawn(move || {
            // パケットの受信用スレッド
            cloned_tcp.receive_handler().unwrap();
        });
        tcp
    }

    fn select_unused_port(&self, rng: &mut ThreadRng) -> Result<u16> {
        for _ in 0..(PORT_RANGE.end - PORT_RANGE.start) {
            let local_port = rng.gen_range(PORT_RANGE);
            let table = self.sockets.read().unwrap();
            // ポートが空いているか確認する。
            if table.keys().all(|k| local_port != k.2) {
                return Ok(local_port);
            }
        }
        anyhow::bail!("no available port found.");
    }

    /// ターゲットに接続し、接続済みソケットIDを返す。
    pub fn connect(&self, addr: Ipv4Addr, port: u16) -> Result<SockID> {
        let mut rng = rand::thread_rng();
        let mut socket = Socket::new(
            get_source_addr_to(addr)?,
            addr,
            self.select_unused_port(&mut rng)?,
            port,
            TcpStatus::SynSent,
        )?;

        socket.send_param.initial_seq = rng.gen_range(1..1 << 31);
        // ここで SYN を送ってる。3 way handshake の最初のセグメント。
        socket.send_tcp_packet(socket.send_param.initial_seq, 0, tcpflags::SYN, &[])?;
        socket.send_param.unacked_seq = socket.send_param.initial_seq;
        // NOTE: SYN セグメントはペイロードを持たないが、確認応答を受け取るために1つインクリメントする。FIN セグメントも同様。
        socket.send_param.next = socket.send_param.initial_seq + 1;
        let mut table = self.sockets.write().unwrap();
        let sock_id = socket.get_sock_id();
        table.insert(sock_id, socket);

        // NOTE: ロックを外してイベントの待機. 受信スレッドがロックを取得できるようにするため。
        drop(table);
        self.wait_event(sock_id, TCPEventKind::ConnectionCompleted);

        Ok(sock_id)
    }
}

/// 宛先IPアドレスに対する送信もとインターフェースのIPアドレスを取得する。
/// iproute2-ss180129 で動作を確認。バージョンによって挙動が変わるかも。
fn get_source_addr_to(addr: Ipv4Addr) -> Result<Ipv4Addr> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!("ip route get {} | grep src", addr))
        .output()?;
    let mut output = str::from_utf8(&output.stdout)?
        .trim()
        .split_ascii_whitespace();
    while let Some(s) = output.next() {
        if s == "src" {
            break;
        }
    }
    let ip = output.next().context("failed to get src ip")?;
    dbg!("source addr", ip);
    ip.parse().context("failed to parse source ip")
}
