//! Local (`AF_UNIX`) sockets for Linux programs: stream, datagram and
//! sequenced-packet sockets, named by a path or in the abstract namespace
//! (a name starting with a NUL byte), `socketpair`, and descriptors passed
//! along with the data (`SCM_RIGHTS`) — how Wayland clients hand over
//! their buffers and how Firefox's processes talk to each other.
//!
//! Each socket has a receive queue of segments (bytes, plus any
//! descriptors sent with them); a connected socket writes into its peer's
//! queue. Names live in a table, not in the file system.

use crate::proc::{readiness, Handle};
use crate::sync::Mutex;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use aurora_abi::err::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Type {
    Stream,
    Dgram,
    SeqPacket,
}

/// What one `send` put in a queue.
pub struct Segment {
    pub data: Vec<u8>,
    pub fds: Vec<Arc<Handle>>,
    /// The sender's name (datagrams sent to a named socket).
    pub from: Option<String>,
    /// Bytes of `data` already read (streams).
    read: usize,
    /// The sending process (`SCM_CREDENTIALS`).
    pid: u32,
}

#[derive(Default)]
struct Queue {
    segs: VecDeque<Segment>,
    bytes: usize,
    /// No socket reads this queue any more (writes fail with EPIPE).
    reader_gone: bool,
    /// Nothing will be written to it any more (reads end, as at end of file).
    writer_gone: bool,
}

/// Bytes a queue holds before writers wait.
const CAPACITY: usize = 256 * 1024;

type QueueRef = Arc<Mutex<Queue>>;

struct State {
    local: Option<String>,
    remote: Option<String>,
    listening: bool,
    backlog: VecDeque<Arc<Socket>>,
    /// Where this socket's writes go (the peer's receive queue).
    tx: Option<QueueRef>,
    /// The process at the other end (`SO_PEERCRED`).
    peer_pid: u32,
    /// Datagram sockets: the default destination (`connect`).
    dest: Option<String>,
    connected: bool,
}

pub struct Socket {
    pub ty: Type,
    rx: QueueRef,
    st: Mutex<State>,
    /// `SO_PASSCRED`: messages read come with the sender's credentials.
    pub passcred: core::sync::atomic::AtomicBool,
}

/// Bound names: paths and abstract names (`@name`).
static NAMES: Mutex<BTreeMap<String, Weak<Socket>>> = Mutex::new(BTreeMap::new());

impl Socket {
    pub fn new(ty: Type) -> Arc<Socket> {
        Arc::new(Socket {
            ty,
            rx: Arc::new(Mutex::new(Queue::default())),
            st: Mutex::new(State {
                local: None,
                remote: None,
                listening: false,
                backlog: VecDeque::new(),
                tx: None,
                peer_pid: 0,
                dest: None,
                connected: false,
            }),
            passcred: core::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Two sockets connected to each other (`socketpair`).
    pub fn pair(ty: Type, pid: u32) -> (Arc<Socket>, Arc<Socket>) {
        let (a, b) = (Socket::new(ty), Socket::new(ty));
        Socket::join(&a, &b, pid, pid);
        (a, b)
    }

    /// Both queues of a connection changed state (one end shut down or went).
    fn notify_ends(rx: &QueueRef, tx: Option<&QueueRef>) {
        readiness::notify(readiness::key(Arc::as_ptr(rx)));
        if let Some(t) = tx {
            readiness::notify(readiness::key(Arc::as_ptr(t)));
        }
    }

    fn join(a: &Arc<Socket>, b: &Arc<Socket>, a_pid: u32, b_pid: u32) {
        {
            let mut s = a.st.lock();
            s.tx = Some(b.rx.clone());
            s.peer_pid = b_pid;
            s.connected = true;
        }
        let mut s = b.st.lock();
        s.tx = Some(a.rx.clone());
        s.peer_pid = a_pid;
        s.connected = true;
    }

    pub fn bind(self: &Arc<Socket>, name: String) -> Result<(), isize> {
        let mut names = NAMES.lock();
        if names.get(&name).is_some_and(|w| w.strong_count() > 0) {
            return Err(EADDRINUSE);
        }
        let mut s = self.st.lock();
        if s.local.is_some() {
            return Err(EINVAL);
        }
        s.local = Some(name.clone());
        names.insert(name, Arc::downgrade(self));
        Ok(())
    }

    pub fn listen(&self) -> Result<(), isize> {
        if self.ty == Type::Dgram {
            return Err(EOPNOTSUPP);
        }
        let mut s = self.st.lock();
        if s.local.is_none() {
            // Linux binds an unnamed socket to an abstract name on listen.
            return Err(EINVAL);
        }
        s.listening = true;
        Ok(())
    }

    /// Connects to the socket bound to `name`.
    pub fn connect(self: &Arc<Socket>, name: String, pid: u32) -> Result<(), isize> {
        let target = NAMES.lock().get(&name).and_then(Weak::upgrade);
        if self.ty == Type::Dgram {
            if target.is_none() {
                return Err(ECONNREFUSED);
            }
            let mut s = self.st.lock();
            s.dest = Some(name.clone());
            s.remote = Some(name);
            return Ok(());
        }
        let listener = target.ok_or(ECONNREFUSED)?;
        if listener.ty != self.ty {
            return Err(EPROTOTYPE);
        }
        if self.st.lock().connected {
            return Err(EISCONN);
        }
        let server = Socket::new(self.ty);
        {
            let mut ls = listener.st.lock();
            if !ls.listening {
                return Err(ECONNREFUSED);
            }
            if ls.backlog.len() >= 128 {
                return Err(EAGAIN);
            }
            server.st.lock().local = Some(name.clone());
            Socket::join(self, &server, pid, 0);
            ls.backlog.push_back(server);
        }
        self.st.lock().remote = Some(name);
        readiness::notify(readiness::key(Arc::as_ptr(&listener)));
        readiness::notify(readiness::key(Arc::as_ptr(self)));
        Ok(())
    }

    /// The next connection on a listening socket.
    pub fn accept(&self, nonblock: bool, pid: u32) -> Result<Arc<Socket>, isize> {
        loop {
            let gen = readiness::generation();
            {
                let mut s = self.st.lock();
                if !s.listening {
                    return Err(EINVAL);
                }
                if let Some(c) = s.backlog.pop_front() {
                    // Tell the client side who accepted it.
                    let _ = pid;
                    return Ok(c);
                }
            }
            wait(gen, &[readiness::key(self)], nonblock)?;
        }
    }

    pub fn peer_pid(&self) -> u32 {
        self.st.lock().peer_pid
    }

    pub fn names(&self) -> (Option<String>, Option<String>) {
        let s = self.st.lock();
        (s.local.clone(), s.remote.clone())
    }

    pub fn listening(&self) -> bool {
        self.st.lock().listening
    }

    /// Sends `data` (and `fds`); `to` names the destination of a datagram.
    /// Returns the bytes taken; `EPIPE` when nobody can read them any more.
    pub fn send(&self, data: &[u8], fds: Vec<Arc<Handle>>, to: Option<String>, nonblock: bool) -> Result<usize, isize> {
        let (dest, tx, local) = {
            let s = self.st.lock();
            (to.or_else(|| s.dest.clone()), s.tx.clone(), s.local.clone())
        };
        let (queue, from) = match (self.ty, dest) {
            (Type::Dgram, Some(name)) => {
                let target = NAMES.lock().get(&name).and_then(Weak::upgrade).ok_or(ECONNREFUSED)?;
                (target.rx.clone(), local)
            }
            _ => (tx.ok_or(ENOTCONN)?, None),
        };
        let mut fds = Some(fds);
        let mut done = 0;
        loop {
            let gen = readiness::generation();
            {
                let mut q = queue.lock();
                if q.reader_gone {
                    return if done > 0 { Ok(done) } else { Err(EPIPE) };
                }
                let room = CAPACITY.saturating_sub(q.bytes);
                let whole = self.ty != Type::Stream;
                if whole && data.len() > CAPACITY {
                    return Err(EMSGSIZE);
                }
                if room > 0 && (!whole || room >= data.len()) {
                    let n = if whole { data.len() } else { room.min(data.len() - done) };
                    let chunk = data[done..done + n].to_vec();
                    q.bytes += chunk.len();
                    q.segs.push_back(Segment {
                        data: chunk,
                        fds: fds.take().unwrap_or_default(),
                        from: from.clone(),
                        read: 0,
                        pid: crate::sched::current_pid(),
                    });
                    done += n;
                    drop(q);
                    readiness::notify(readiness::key(Arc::as_ptr(&queue)));
                    if whole || done == data.len() {
                        return Ok(done);
                    }
                    continue;
                }
            }
            if done > 0 && nonblock {
                return Ok(done);
            }
            wait(gen, &[readiness::key(Arc::as_ptr(&queue))], nonblock)?;
        }
    }

    /// Receives into `out`: bytes, descriptors, the sender's name, and
    /// whether a datagram was cut short. `Ok((0, ..))` at end of file.
    pub fn recv(&self, out: &mut [u8], peek: bool, nonblock: bool) -> Result<Received, isize> {
        loop {
            let gen = readiness::generation();
            {
                let mut q = self.rx.lock();
                if !q.segs.is_empty() {
                    let r = self.take(&mut q, out, peek);
                    drop(q);
                    if !peek {
                        // Room for whoever waits to write.
                        readiness::notify(readiness::key(Arc::as_ptr(&self.rx)));
                    }
                    return Ok(r);
                }
                if q.writer_gone || (self.ty != Type::Dgram && !self.st.lock().connected && !self.listening()) {
                    return Ok(Received::default());
                }
            }
            wait(gen, &[readiness::key(Arc::as_ptr(&self.rx)), readiness::key(self)], nonblock)?;
        }
    }

    fn take(&self, q: &mut Queue, out: &mut [u8], peek: bool) -> Received {
        let mut r = Received::default();
        if self.ty != Type::Stream {
            let seg = if peek { None } else { q.segs.pop_front() };
            let s = seg.as_ref().or(q.segs.front()).unwrap();
            let n = s.data.len().min(out.len());
            out[..n].copy_from_slice(&s.data[..n]);
            r.len = n;
            r.truncated = s.data.len() > n;
            r.from = s.from.clone();
            r.pid = s.pid;
            if let Some(s) = seg {
                q.bytes -= s.data.len();
                r.fds = s.fds;
            }
            return r;
        }
        // A stream: across segments, stopping where descriptors come along.
        let mut i = 0;
        while r.len < out.len() && i < q.segs.len() {
            let seg = &mut q.segs[i];
            if !seg.fds.is_empty() && r.len > 0 && seg.read == 0 {
                break;
            }
            let avail = &seg.data[seg.read..];
            if r.len == 0 {
                r.pid = seg.pid;
            }
            let n = avail.len().min(out.len() - r.len);
            out[r.len..r.len + n].copy_from_slice(&avail[..n]);
            r.len += n;
            let carried = !seg.fds.is_empty();
            if !peek {
                seg.read += n;
                r.fds.append(&mut seg.fds);
            }
            i += 1;
            if carried {
                break;
            }
        }
        if !peek {
            while q.segs.front().is_some_and(|s| s.read == s.data.len()) {
                q.segs.pop_front();
            }
            q.bytes -= r.len;
        }
        r
    }

    /// Bytes waiting (`FIONREAD`): all of them for a stream, the next
    /// message's for the others.
    pub fn available(&self) -> usize {
        let q = self.rx.lock();
        match self.ty {
            Type::Stream => q.bytes,
            _ => q.segs.front().map_or(0, |s| s.data.len()),
        }
    }

    /// What to wait on for a change in [`Socket::readiness`] (see `proc::readiness`).
    pub fn wait_keys(&self, keys: &mut Vec<usize>) {
        keys.push(readiness::key(self));
        keys.push(readiness::key(Arc::as_ptr(&self.rx)));
        if let Some(t) = &self.st.lock().tx {
            keys.push(readiness::key(Arc::as_ptr(t)));
        }
    }

    /// Whether a read and a write would go ahead, and whether the other end is gone.
    pub fn readiness(&self) -> (bool, bool, bool) {
        let s = self.st.lock();
        if s.listening {
            return (!s.backlog.is_empty(), false, false);
        }
        let tx = s.tx.clone();
        let connected = s.connected;
        drop(s);
        let rq = self.rx.lock();
        let readable = !rq.segs.is_empty() || rq.writer_gone;
        let ended = rq.writer_gone;
        drop(rq);
        let (writable, gone) = match tx {
            Some(t) => {
                let q = t.lock();
                (q.reader_gone || q.bytes < CAPACITY, q.reader_gone)
            }
            None => (self.ty == Type::Dgram, !connected && self.ty != Type::Dgram),
        };
        (readable, writable, ended || gone)
    }

    /// `shutdown`: no more reading (0), writing (1) or both (2).
    pub fn shutdown(&self, how: u64) -> Result<(), isize> {
        let s = self.st.lock();
        if how != 1 {
            self.rx.lock().reader_gone = true;
        }
        if how != 0 {
            if let Some(t) = &s.tx {
                t.lock().writer_gone = true;
            }
        }
        let tx = s.tx.clone();
        drop(s);
        Self::notify_ends(&self.rx, tx.as_ref());
        Ok(())
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let s = self.st.lock();
        if let Some(t) = &s.tx {
            t.lock().writer_gone = true;
        }
        let local = s.local.clone();
        let tx = s.tx.clone();
        drop(s);
        if let Some(name) = local {
            let mut names = NAMES.lock();
            if names.get(&name).is_some_and(|w| w.strong_count() == 0) {
                names.remove(&name);
            }
        }
        let mut q = self.rx.lock();
        q.reader_gone = true;
        // Descriptors in flight to nobody close now.
        q.segs.clear();
        drop(q);
        Socket::notify_ends(&self.rx, tx.as_ref());
    }
}

/// What a receive got.
#[derive(Default)]
pub struct Received {
    pub len: usize,
    pub fds: Vec<Arc<Handle>>,
    pub from: Option<String>,
    pub truncated: bool,
    /// Who sent it.
    pub pid: u32,
}

/// Waits for the next change to `keys` (or fails at once without blocking).
fn wait(gen: u64, keys: &[usize], nonblock: bool) -> Result<(), isize> {
    if nonblock {
        return Err(EAGAIN);
    }
    if crate::proc::interrupted() {
        return Err(EINTR);
    }
    readiness::wait(gen, keys, 1000);
    Ok(())
}
