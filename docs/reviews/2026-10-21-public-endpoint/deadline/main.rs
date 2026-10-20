use std::{io::{Read, Write},net::{TcpListener,TcpStream},sync::Arc,time::{Duration,Instant}};
use rustls::{ClientConfig,ClientConnection,RootCertStore,StreamOwned,pki_types::ServerName};
fn main() {
 let l=TcpListener::bind("127.0.0.1:0").unwrap(); let addr=l.local_addr().unwrap();
 let worker=std::thread::spawn(move || { let (mut s,_)=l.accept().unwrap(); let mut buf=[0;4096]; s.read(&mut buf).unwrap();
 // Valid handshake record header; payload trickles without ever completing the record.
 for b in [22,3,3,0,100,0,0,0] { std::thread::sleep(Duration::from_millis(500)); if s.write_all(&[b]).is_err() { break; } }
 });
 let tcp=TcpStream::connect(addr).unwrap();tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();tcp.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
 let conf=ClientConfig::builder().with_root_certificates(RootCertStore::empty()).with_no_client_auth();
 let tls=ClientConnection::new(Arc::new(conf),ServerName::try_from("test.invalid").unwrap()).unwrap(); let mut stream=StreamOwned::new(tls,tcp);
 let started=Instant::now();let r=stream.write_all(b"POST /dns-query HTTP/1.1\r\n");println!("budget=3s elapsed={:?} result={r:?}",started.elapsed());worker.join().unwrap();
}
