use crate::packet::TCPPacket;
use crate::tcpflags;
use anyhow::{Context, Result};
use pnet::packet::{ip::IpNextHeaderProtocols, Packet};
use pnet::transport::{self, TransportChannelType, TransportProtocol, TransportSender};
use pnet::util;
use std::collections::VecDeque;
use std::fmt::{self, Display};
use std::net::{IpAddr, Ipv4Addr};
use std::time::SystemTime;

const SOCKET_BUFFER_SIZE: usize = 4380;

/// (loal_addr, remote_addr, local_port, remote_port) のタプルでコネクションを識別する。
/// ソケットはそのエンドポイントになる。
/// プロトコル種別を加えて、5 tuple と呼ばれるが、ここでは TCP のみを扱うので4つで十分。
#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy)]
pub struct SockID(pub Ipv4Addr, pub Ipv4Addr, pub u16, pub u16);

pub struct Socket {
    pub local_addr: Ipv4Addr,
    pub remote_addr: Ipv4Addr,
    pub local_port: u16,
    pub remote_port: u16,
    pub send_param: SendParam,
    pub recv_param: RecvParam,
    pub status: TcpStatus,

    // 到着したデータを一度保管する。TCPセグメントは通信の途中で順番が入れ替わったり失われたり色々あるので。
    pub recv_buffer: Vec<u8>,

    pub retransmission_queue: VecDeque<RetransmissionQueueEntry>,

    // 接続済みソケットを保持するキュー。りすにんぐそけっとのみ使用。
    pub connected_connection_euque: VecDeque<SockID>,

    // 生成元のリスニングソケット。接続済みソケットのみ使用。
    pub listening_socket: Option<SockID>,

    pub sender: TransportSender,
}

#[derive(Clone, Debug)]
pub struct SendParam {
    pub unacked_seq: u32, // 送信後、まだ ack されていない seq の先頭
    pub next: u32,        // 次の送信
    pub window: u16,      // 送信ウィンドウサイズ
    pub initial_seq: u32, // 初期送信 seq
}

#[derive(Clone, Debug)]
pub struct RecvParam {
    pub next: u32,        // 次に受診する seq
    pub window: u16,      // 受信ウィンドウサイズ
    pub initial_seq: u32, // 初期受信 seq
    pub tail: u32,        // 受信 seq の最後尾
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub enum TcpStatus {
    Listen,
    SynSent,
    SynRcvd,
    Established,
    FinWait1,
    FinWait2,
    TimeWait,
    CloseWait,
    LastAck,
}

impl Display for TcpStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            TcpStatus::Listen => write!(f, "LISTEN"),
            TcpStatus::SynSent => write!(f, "SYNSENT"),
            TcpStatus::SynRcvd => write!(f, "SYNRCVD"),
            TcpStatus::Established => write!(f, "ESTABLISHED"),
            TcpStatus::FinWait1 => write!(f, "FINWAIT1"),
            TcpStatus::FinWait2 => write!(f, "FINWAIT2"),
            TcpStatus::TimeWait => write!(f, "TIMEWAIT"),
            TcpStatus::CloseWait => write!(f, "CLOSEWAIT"),
            TcpStatus::LastAck => write!(f, "LASTACK"),
        }
    }
}

impl Socket {
    pub fn new(
        local_addr: Ipv4Addr,
        remote_addr: Ipv4Addr,
        local_port: u16,
        remote_port: u16,
        status: TcpStatus,
    ) -> Result<Self> {
        // ここでtransport layerにおけるやり取りをするためのchannelを作成してるっぽい。
        // TCP 接続とは違うだろう。ポートとか指定してないので。
        // 内部的に Raw Socket を用いており、TCPのフォーマットに成形されたバイト列を書き込んで送信ができるとのこと。
        let (sender, _) = transport::transport_channel(
            65535,
            TransportChannelType::Layer4(TransportProtocol::Ipv4(IpNextHeaderProtocols::Tcp)),
        )?;

        Ok(Self {
            local_addr,
            remote_addr,
            local_port,
            remote_port,
            send_param: SendParam {
                unacked_seq: 0,
                initial_seq: 0,
                next: 0,
                window: SOCKET_BUFFER_SIZE as u16,
            },
            recv_param: RecvParam {
                initial_seq: 0,
                next: 0,
                window: SOCKET_BUFFER_SIZE as u16,
                tail: 0,
            },
            status,
            recv_buffer: vec![0; SOCKET_BUFFER_SIZE],
            retransmission_queue: VecDeque::new(),
            connected_connection_euque: VecDeque::new(),
            listening_socket: None,
            sender,
        })
    }

    pub fn send_tcp_packet(
        &mut self,
        seq: u32,
        ack: u32,
        flag: u8,
        payload: &[u8],
    ) -> Result<usize> {
        let mut tcp_packet = TCPPacket::new(payload.len());
        tcp_packet.set_src(self.local_port);
        tcp_packet.set_dest(self.remote_port);
        tcp_packet.set_seq(seq);
        tcp_packet.set_ack(ack);
        // NOTE: 今回はオプションフィールドは使わない。
        // NOTE: よって、ヘッダーは 32-bit words * 5 分あることになり、その直後に data(payload) が始まることになる。
        // NOTE: よって、data offset は 5 になる。詳しくは[RFC9293](https://datatracker.ietf.org/doc/html/rfc9293)を参照。
        tcp_packet.set_data_offset(5);
        tcp_packet.set_flag(flag);
        tcp_packet.set_window_size(self.recv_param.window);
        tcp_packet.set_payload(payload);
        tcp_packet.set_checksum(util::ipv4_checksum(
            &tcp_packet.packet(),
            8,
            &[],
            &self.local_addr,
            &self.remote_addr,
            IpNextHeaderProtocols::Tcp,
        ));
        let sent_size = self
            .sender
            .send_to(tcp_packet.clone(), IpAddr::V4(self.remote_addr))
            .context(format!("failed to send: \n{:?}", tcp_packet))?;

        dbg!("sent", &tcp_packet);
        // もし送信先から確認応答がこなかった場合は再送する必要がある。
        // なので、送信直後のこのタイミングでエンキューする。
        // ただし、ペイロードを持たないACKセグメントは再送対象にはなりません。ACKセグメントのACKセグメントというように、無限に確認応答が必要になる。
        // 再送対象になるのは、ペイロードが存在しているか、ACKセグメントでないセグメントです。
        // 例：SYNセグメント、SYN|ACKセグメント、ペイロードをのせたACKセグメント
        if payload.is_empty() && tcp_packet.get_flag() == tcpflags::ACK {
            return Ok(sent_size);
        }
        self.retransmission_queue
            .push_back(RetransmissionQueueEntry::new(tcp_packet));
        Ok(sent_size)
    }

    pub fn get_sock_id(&self) -> SockID {
        SockID(
            self.local_addr,
            self.remote_addr,
            self.local_port,
            self.remote_port,
        )
    }
}

#[derive(Clone, Debug)]
pub struct RetransmissionQueueEntry {
    pub packet: TCPPacket,
    pub latest_transmission_time: SystemTime,
    pub transmission_count: u8,
}

impl RetransmissionQueueEntry {
    fn new(packet: TCPPacket) -> Self {
        Self {
            packet,
            latest_transmission_time: SystemTime::now(),
            transmission_count: 1,
        }
    }
}
