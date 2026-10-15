//! TCP side of the serial cartridge: the C64's SwiftLink/Turbo232 talks to a
//! socket as if it were a modem line (a "null modem over TCP", like VICE's
//! `-rsdev1 ip232`-less raw mode). Two spec forms:
//!
//! * `connect:host:port` -- dial out; retried every 2 s while down, and again
//!   after the peer hangs up.
//! * `listen:port` -- accept one peer at a time on `127.0.0.1:port` only.
//!
//! Background threads own the sockets; [`SerialNet::pump`] (called from the
//! emulation step, never blocking) moves bytes between them and the machine's
//! ACIA queues and sets the carrier line from the connection state.

use c64::C64;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    Connect(String),
    Listen(u16),
}

impl Spec {
    pub fn parse(text: &str) -> Result<Option<Spec>, String> {
        let text = text.trim();
        if text.is_empty() || text == "off" {
            return Ok(None);
        }
        if let Some(rest) = text.strip_prefix("connect:") {
            let ok = rest
                .rsplit_once(':')
                .is_some_and(|(h, p)| !h.is_empty() && p.parse::<u16>().is_ok());
            if !ok {
                return Err(format!("serial_net \"{text}\": expected connect:host:port"));
            }
            return Ok(Some(Spec::Connect(rest.to_string())));
        }
        if let Some(rest) = text.strip_prefix("listen:") {
            return rest
                .parse::<u16>()
                .map(|p| Some(Spec::Listen(p)))
                .map_err(|_| format!("serial_net \"{text}\": expected listen:port"));
        }
        Err(format!(
            "serial_net \"{text}\": expected connect:host:port or listen:port"
        ))
    }
}

#[derive(Default)]
struct Shared {
    rx: Mutex<VecDeque<u8>>,
    stream: Mutex<Option<TcpStream>>,
    connected: AtomicBool,
    stop: AtomicBool,
}

pub struct SerialNet {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
    /// Pump bookkeeping: steps since the last transfer.
    steps: u32,
}

/// How many emulated instructions pass between transfers (at ~1 MHz that is a
/// few hundred transfers per second, far finer than any serial rate).
const PUMP_INTERVAL: u32 = 256;
/// Bytes handed to the ACIA per transfer, so a flood cannot starve the CPU.
const RX_BATCH: usize = 64;

fn reader(shared: Arc<Shared>, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let mut buf = [0u8; 512];
    while !shared.stop.load(Ordering::Relaxed) {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Ok(mut q) = shared.rx.lock() {
                    q.extend(&buf[..n]);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    shared.connected.store(false, Ordering::Relaxed);
    if let Ok(mut s) = shared.stream.lock() {
        *s = None;
    }
}

fn adopt(shared: &Arc<Shared>, stream: TcpStream) -> Option<std::thread::JoinHandle<()>> {
    let _ = stream.set_nodelay(true);
    let read_half = stream.try_clone().ok()?;
    *shared.stream.lock().ok()? = Some(stream);
    shared.connected.store(true, Ordering::Relaxed);
    let sh = shared.clone();
    std::thread::Builder::new()
        .name("c64-serial-rx".into())
        .spawn(move || reader(sh, read_half))
        .ok()
}

impl SerialNet {
    pub fn start(spec: Spec) -> Result<SerialNet, String> {
        let shared = Arc::new(Shared::default());
        let sh = shared.clone();
        let worker = match spec {
            Spec::Listen(port) => {
                let listener = TcpListener::bind(("127.0.0.1", port))
                    .map_err(|e| format!("serial_net listen:{port}: {e}"))?;
                listener
                    .set_nonblocking(true)
                    .map_err(|e| format!("serial_net listen:{port}: {e}"))?;
                std::thread::Builder::new()
                    .name("c64-serial-accept".into())
                    .spawn(move || {
                        let mut reader_handle: Option<std::thread::JoinHandle<()>> = None;
                        while !sh.stop.load(Ordering::Relaxed) {
                            if !sh.connected.load(Ordering::Relaxed)
                                && let Ok((stream, _)) = listener.accept()
                            {
                                let _ = stream.set_nonblocking(false);
                                if let Some(h) = reader_handle.take() {
                                    let _ = h.join();
                                }
                                reader_handle = adopt(&sh, stream);
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        if let Some(h) = reader_handle {
                            let _ = h.join();
                        }
                    })
            }
            Spec::Connect(addr) => std::thread::Builder::new()
                .name("c64-serial-connect".into())
                .spawn(move || {
                    let mut reader_handle: Option<std::thread::JoinHandle<()>> = None;
                    while !sh.stop.load(Ordering::Relaxed) {
                        if !sh.connected.load(Ordering::Relaxed) {
                            if let Some(h) = reader_handle.take() {
                                let _ = h.join();
                            }
                            let target = addr.to_socket_addrs().ok().and_then(|mut a| a.next());
                            if let Some(t) = target
                                && let Ok(stream) =
                                    TcpStream::connect_timeout(&t, Duration::from_secs(2))
                            {
                                reader_handle = adopt(&sh, stream);
                            }
                            // Wait before the next attempt (or poll for stop).
                            for _ in 0..40 {
                                if sh.stop.load(Ordering::Relaxed)
                                    || sh.connected.load(Ordering::Relaxed)
                                {
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(50));
                            }
                        } else {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                    if let Some(h) = reader_handle {
                        let _ = h.join();
                    }
                }),
        }
        .map_err(|e| format!("serial_net: could not start a thread: {e}"))?;
        Ok(SerialNet {
            shared,
            threads: vec![worker],
            steps: 0,
        })
    }

    /// Whether a peer is connected right now.
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::Relaxed)
    }

    /// Call once per emulated instruction; transfers every `PUMP_INTERVAL`.
    pub fn tick(&mut self, machine: &mut C64) {
        self.steps += 1;
        if self.steps >= PUMP_INTERVAL {
            self.steps = 0;
            self.pump(machine);
        }
    }

    /// Moves bytes both ways and updates the carrier. Never blocks.
    pub fn pump(&mut self, machine: &mut C64) {
        let connected = self.connected();
        machine.serial_set_carrier(connected);
        let tx = machine.serial_take_tx();
        if !tx.is_empty()
            && connected
            && let Ok(mut guard) = self.shared.stream.lock()
            && let Some(stream) = guard.as_mut()
            && stream.write_all(&tx).is_err()
        {
            *guard = None;
            self.shared.connected.store(false, Ordering::Relaxed);
        }
        // Keep only a short backlog in the chip so the line behaves like a
        // paced serial port rather than dumping a whole buffer at once.
        if machine.serial_rx_backlog() < RX_BATCH
            && let Ok(mut q) = self.shared.rx.lock()
        {
            for _ in 0..RX_BATCH {
                match q.pop_front() {
                    Some(b) => machine.serial_push_rx(b),
                    None => break,
                }
            }
        }
    }
}

impl Drop for SerialNet {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Ok(mut s) = self.shared.stream.lock()
            && let Some(stream) = s.take()
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse_and_reject_garbage() {
        assert_eq!(Spec::parse("").unwrap(), None);
        assert_eq!(Spec::parse("off").unwrap(), None);
        assert_eq!(
            Spec::parse("connect:bbs.example.org:6400").unwrap(),
            Some(Spec::Connect("bbs.example.org:6400".into()))
        );
        assert_eq!(
            Spec::parse("listen:2323").unwrap(),
            Some(Spec::Listen(2323))
        );
        assert!(Spec::parse("connect:nohost").is_err());
        assert!(Spec::parse("listen:abc").is_err());
        assert!(Spec::parse("tcp://x").is_err());
    }

    fn machine() -> C64 {
        use c64::C64;
        C64::new(vec![0xEA; 8192], vec![0xEA; 8192], vec![0; 4096]).unwrap()
    }

    #[test]
    fn a_tcp_peer_talks_to_a_program_through_the_swiftlink() {
        use machine::Machine;
        let port = {
            let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            l.local_addr().unwrap().port()
        };
        let mut c64 = machine();
        c64.attach_serial(false, 0xDE).unwrap();
        let mut net = SerialNet::start(Spec::Listen(port)).unwrap();
        // 38400 bps 8N1, DTR on, no interrupts; echo every received byte.
        let code = [
            0xA9, 0x1F, 0x8D, 0x03, 0xDE, 0xA9, 0x03, 0x8D, 0x02, 0xDE, // init
            0xAD, 0x01, 0xDE, 0x29, 0x08, 0xF0, 0xF9, // wait for a byte
            0xAD, 0x00, 0xDE, 0x8D, 0x00, 0xDE, // echo it
            0x4C, 0x0A, 0xC0,
        ];
        c64.load(0xC000, &code);
        c64.set_pc(0xC000);

        let mut peer = TcpStream::connect(("127.0.0.1", port)).unwrap();
        peer.set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        peer.write_all(b"hello").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut got = Vec::new();
        while got.len() < 5 && std::time::Instant::now() < deadline {
            for _ in 0..20_000 {
                c64.step();
                net.tick(&mut c64);
            }
            let mut buf = [0u8; 16];
            if let Ok(n) = peer.read(&mut buf) {
                got.extend_from_slice(&buf[..n]);
            }
        }
        assert_eq!(got, b"hello");
        assert!(net.connected());
        drop(peer);
        // Hang-up drops the carrier.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while net.connected() && std::time::Instant::now() < deadline {
            net.pump(&mut c64);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!net.connected());
    }
}
