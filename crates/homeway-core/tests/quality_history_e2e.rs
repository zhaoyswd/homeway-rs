//! 真实私有出口：两设备 QUIC DATAGRAM → intercept TCP 重拨 → 本机 echo → 历史报告。
//! `tools/quality-e2e.sh` 驱动；不使用生产出口或系统 TUN。
#![cfg(unix)]

use std::io::{Read, Write};
use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use homeway_core::identity::Identity;
use homeway_core::server::quality::{read_report, QualityQuery};
use homeway_core::token::{self, EndpointKind};
use homeway_quic::{Candidate, Cmd, Island, IslandConfig, IslandCredential, IslandReply, RpkPublicKey, TokenSecret, Via};
use smoltcp::iface::{Config as IfaceConfig, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::wire::{HardwareAddress, IpCidr};
use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer};
use smoltcp::time::Instant as SmolInstant;

fn stamp() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn call<T>(island: &Island, command: impl FnOnce(IslandReply<T>) -> Cmd) -> T {
    let (tx, rx) = mpsc::channel();
    island.tx().send(command(tx)).unwrap();
    rx.recv_timeout(Duration::from_secs(10)).expect("命令必须有界完成").unwrap()
}

// 测试端自持包队列，生产客户端不引用出口协议栈包装。
#[derive(Default)]
struct PacketDevice { input: VecDeque<Vec<u8>>, output: VecDeque<Vec<u8>> }
struct PacketRx(Vec<u8>);
struct PacketTx<'a>(&'a mut VecDeque<Vec<u8>>);
impl phy::RxToken for PacketRx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R { f(&self.0) }
}
impl phy::TxToken for PacketTx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut packet = vec![0; len];
        let result = f(&mut packet);
        self.0.push_back(packet);
        result
    }
}
impl phy::Device for PacketDevice {
    type RxToken<'a> = PacketRx;
    type TxToken<'a> = PacketTx<'a>;
    fn receive(&mut self, _: SmolInstant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let packet = self.input.pop_front()?;
        Some((PacketRx(packet), PacketTx(&mut self.output)))
    }
    fn transmit(&mut self, _: SmolInstant) -> Option<Self::TxToken<'_>> { Some(PacketTx(&mut self.output)) }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = 1280;
        caps
    }
}
impl PacketDevice {
    fn drain_tx(&mut self, out: &mut Vec<Vec<u8>>) { out.extend(self.output.drain(..)); }
}
struct TestNetwork { iface: Interface, device: PacketDevice, sockets: SocketSet<'static> }
impl TestNetwork {
    fn new(ip: Ipv4Addr) -> Self {
        let mut device = PacketDevice::default();
        let mut iface = Interface::new(IfaceConfig::new(HardwareAddress::Ip), &mut device, SmolInstant::from_millis(0));
        iface.update_ip_addrs(|addrs| addrs.push(IpCidr::new(ip.into(), 32)).unwrap());
        iface.routes_mut().add_default_ipv4_route(Ipv4Addr::new(100,64,255,1)).unwrap();
        Self { iface, device, sockets: SocketSet::new(vec![]) }
    }
    fn inject(&mut self, packet: &[u8]) { self.device.input.push_back(packet.to_vec()); }
}

struct Client {
    island: Island,
    _tun: UnixDatagram,
    peer: UnixDatagram,
    stack: TestNetwork,
    handle: SocketHandle,
    device: String,
    payload: Vec<u8>,
    sent: bool,
    received: Vec<u8>,
    close: bool,
    start: Instant,
}

impl Client {
    fn new(tok: &token::Token, port: u16, payload: &[u8]) -> Self {
        let id = Identity::ephemeral().unwrap();
        let device = id.dev_tag().as_bytes().iter().map(|v| format!("{v:02x}")).collect();
        let ip = homeway_core::tunnel_addr::derive_tun_ip(&tok.secret, &id.public_key());
        let mut cfg = IslandConfig::new(IslandCredential::new(
            TokenSecret::from_bytes(*tok.secret.as_bytes()), id.public_key(), *id.dev_tag().as_bytes(),
            RpkPublicKey::from_bytes(*tok.rpk.as_ref().unwrap().as_bytes()),
        ));
        cfg.bind = Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)));
        let island = Island::start(Arc::new(|_| {}), Arc::new(|_| {}), cfg).unwrap();
        let candidates = tok.endpoints.iter().filter(|e| e.kind == EndpointKind::Quic).map(|e| {
            let a: SocketAddr = e.addr.parse().unwrap();
            Candidate { addr: SocketAddr::from((Ipv4Addr::LOCALHOST, a.port())), via: Via::Direct }
        }).collect();
        let _: homeway_quic::RaceOutcome = call(&island, |reply| Cmd::Connect {
            cands: candidates, budget: Duration::from_secs(5), demote_adopted: false, reply,
        });
        let (tun, peer) = UnixDatagram::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        call(&island, |reply| Cmd::TunAttach { fd: tun.as_raw_fd(), mtu: 1280, reply });
        let mut stack = TestNetwork::new(ip);
        let mut sock = TcpSocket::new(SocketBuffer::new(vec![0; 65536]), SocketBuffer::new(vec![0; 65536]));
        sock.set_nagle_enabled(false);
        let handle = stack.sockets.add(sock);
        stack.sockets.get_mut::<TcpSocket>(handle).connect(stack.iface.context(), (Ipv4Addr::LOCALHOST, port), 41001).unwrap();
        Self { island, _tun: tun, peer, stack, handle, device, payload: payload.to_vec(), sent: false,
            received: Vec::new(), close: false, start: Instant::now() }
    }

    fn step(&mut self) {
        let socket = self.stack.sockets.get_mut::<TcpSocket>(self.handle);
        if !self.sent && socket.can_send() {
            assert_eq!(socket.send_slice(&self.payload).unwrap(), self.payload.len());
            self.sent = true;
        }
        if socket.can_recv() {
            let mut buf = [0; 4096];
            let n = socket.recv_slice(&mut buf).unwrap();
            self.received.extend_from_slice(&buf[..n]);
        }
        if self.close { socket.close(); }
        self.stack.iface.poll(SmolInstant::from_millis(self.start.elapsed().as_millis() as i64),
            &mut self.stack.device, &mut self.stack.sockets);
        let mut packets = Vec::new();
        self.stack.device.drain_tx(&mut packets);
        for packet in packets { self.peer.send(&packet).unwrap(); }
        let mut buf = [0; 2048];
        loop {
            match self.peer.recv(&mut buf) {
                Ok(n) => self.stack.inject(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("TUN 回程错误：{e}"),
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) { self.island.stop_within(Instant::now() + Duration::from_secs(2)); }
}

#[test]
#[ignore = "真实私有出口；tools/quality-e2e.sh 驱动"]
fn two_devices_transit_bytes_are_isolated_in_history() {
    let tok = token::decode(&std::env::var("HOMEWAY_QUALITY_E2E_TOKEN").expect("私有出口凭据")).unwrap();
    let state = PathBuf::from(std::env::var("HOMEWAY_QUALITY_E2E_STATE").expect("私有出口state"));
    assert!(state.starts_with("/tmp"), "只允许私有实例");
    let from = stamp().saturating_sub(1000);
    let echo = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = echo.local_addr().unwrap().port();
    echo.set_nonblocking(true).unwrap();
    let echo_thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut jobs = Vec::new();
        while jobs.len() < 2 && Instant::now() < deadline {
            match echo.accept() {
                Ok((mut socket, _)) => jobs.push(std::thread::spawn(move || {
                    socket.set_nonblocking(false).unwrap();
                    socket.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
                    socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut buf = [0; 4096];
                    // TCP 可短读；循环回显直到应用关闭，保留真实长连接。
                    loop {
                        let n = socket.read(&mut buf).unwrap();
                        if n == 0 { break; }
                        std::thread::sleep(Duration::from_millis(40));
                        socket.write_all(&buf[..n]).unwrap();
                    }
                })),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(2)),
                Err(e) => panic!("echo accept: {e}"),
            }
        }
        assert_eq!(jobs.len(), 2);
        for job in jobs { job.join().unwrap(); }
    });
    let mut clients = [Client::new(&tok, port, b"device-one-quality"), Client::new(&tok, port, b"device-two-quality-longer")];
    let deadline = Instant::now() + Duration::from_secs(8);
    while clients.iter().any(|c| c.received != c.payload) && Instant::now() < deadline {
        for client in &mut clients { client.step(); }
        std::thread::sleep(Duration::from_millis(2));
    }
    for client in &clients { assert_eq!(client.received, client.payload, "真实隧道 TCP echo"); }
    clients[0].close = true;
    let until = Instant::now() + Duration::from_secs(6);
    while Instant::now() < until {
        for client in &mut clients { client.step(); }
        std::thread::sleep(Duration::from_millis(3));
    }
    clients[1].close = true;
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        for client in &mut clients { client.step(); }
        let report = read_report(&state, QualityQuery { from_ms: from, to_ms: stamp() + 1, device: None }).unwrap();
        let matching: Vec<_> = report.targets.iter().filter(|t| t.target == format!("127.0.0.1:{port}")).collect();
        if matching.len() == 2 && matching.iter().all(|t| t.closed_flows == 1) {
            assert!(report.coverage.heartbeat_samples > 0);
            for client in &clients {
                let filtered = read_report(&state, QualityQuery { from_ms: from, to_ms: stamp() + 1, device: Some(client.device.clone()) }).unwrap();
                assert_eq!(filtered.targets.len(), 1, "同目标两设备必须隔离");
                let target = &filtered.targets[0];
                assert_eq!(target.device.as_deref(), Some(client.device.as_str()));
                assert_eq!(target.observed_up_bytes, client.payload.len() as u64, "长短流不得重复计量");
                assert_eq!(target.observed_down_bytes, client.payload.len() as u64);
                assert_eq!(target.tcp_dial_observations, 1);
                assert_eq!(target.tcp_dial_failures, 0);
                assert!(target.first_upstream_wait_ms_p95.is_some());
                assert!(filtered.devices.iter().all(|d| d.device == client.device));
            }
            println!("[quality-e2e] 两设备/同目标/长短流/真实TCP字节对账/设备过滤通过");
            break;
        }
        assert!(Instant::now() < deadline, "历史未结算：{}", report.render_text());
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(clients);
    echo_thread.join().unwrap();
}
