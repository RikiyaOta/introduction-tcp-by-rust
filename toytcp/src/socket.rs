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

/// まだ利用しない
pub enum TcpStatus {}

impl Socket {
    pub fn new(
        local_addr: Ipv4Addr,
        remote_addr: Ipv4Addr,
        local_port: u16,
        remote_port: u16,
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
            sender,
        })
    }

    pub fn send_tcp_packet(&mut self, flag: u8, payload: &[u8]) -> Result<usize> {
        let mut tcp_packet = TCPPacket::new(payload.len());
        tcp_packet.set_src(self.local_port);
        tcp_packet.set_dest(self.remote_port);
        tcp_packet.set_flag(flag);
        let sent_size = self
            .sender
            .send_to(tcp_packet.clone(), IpAddr::V4(self.remote_addr))
            .unwrap();

        Ok(sent_size)
    }

    pub fn get_sock_id(self) -> SockID {
        SockID(
            self.local_addr,
            self.remote_addr,
            self.local_port,
            self.remote_port,
        )
    }
}
