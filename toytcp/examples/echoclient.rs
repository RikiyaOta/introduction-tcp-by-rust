use anyhow::Result;
use std::{env, io, net::Ipv4Addr, str};
use toytcp::tcp::TCP;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let addr: Ipv4Addr = args[1].parse()?;
    let port: u16 = args[2].parse()?;

    let _ = echo_client(addr, port);

    Ok(())
}

/**
 * 今はまだ connect するだけ。
 */
fn echo_client(remote_addr: Ipv4Addr, remote_port: u16) -> Result<()> {
    let tcp = TCP::new();
    let sock_id = tcp.connect(remote_addr, remote_port)?;

    loop {
        // connect した後に stdin で受け取った文字列を send するだけ。
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        // 実験：入力した文字列を増大させて送ってみる（window が効いていることを確認するため）
        // tcp.send(sock_id, input.as_bytes())?;
        loop {
            tcp.send(sock_id, input.repeat(2000).as_bytes())?;
        }
    }
}
