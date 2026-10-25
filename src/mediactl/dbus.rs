//! A small D-Bus client, just enough for the MPRIS service: connect to the
//! session bus over a unix socket, authenticate (SASL EXTERNAL), and
//! (de)marshal messages. Built on std so media keys don't cost a D-Bus
//! crate stack (zbus) or a libdbus link.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

pub const METHOD_CALL: u8 = 1;
pub const METHOD_RETURN: u8 = 2;
pub const ERROR: u8 = 3;
pub const SIGNAL: u8 = 4;
/// Header flag: the caller does not want a reply.
pub const NO_REPLY_EXPECTED: u8 = 0x1;

const BUS_NAME: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
/// The spec caps messages at 128 MiB.
const MAX_MESSAGE: usize = 128 << 20;
/// Nesting limit for containers and variants (the spec allows 64).
const MAX_DEPTH: usize = 64;

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("D-Bus: {msg}"))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Byte(u8),
    Bool(bool),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F64(f64),
    Str(String),
    Path(String),
    Sig(String),
    /// Element signature and elements.
    Array(String, Vec<Value>),
    Struct(Vec<Value>),
    DictEntry(Box<Value>, Box<Value>),
    Variant(Box<Value>),
}

impl Value {
    /// `a{sv}` from property-style entries.
    pub fn dict(entries: Vec<(&str, Value)>) -> Self {
        Self::Array(
            "{sv}".into(),
            entries
                .into_iter()
                .map(|(k, v)| {
                    Self::DictEntry(Box::new(Self::Str(k.into())), Box::new(Self::variant(v)))
                })
                .collect(),
        )
    }

    pub fn strings(items: Vec<String>) -> Self {
        Self::Array("s".into(), items.into_iter().map(Self::Str).collect())
    }

    pub fn variant(v: Value) -> Self {
        Self::Variant(Box::new(v))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) | Self::Path(s) | Self::Sig(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Self::U32(n) => Some(*n),
            _ => None,
        }
    }

    /// The value inside a variant (or the value itself).
    pub fn inner(&self) -> &Value {
        match self {
            Self::Variant(v) => v.inner(),
            v => v,
        }
    }

    pub fn write_signature(&self, out: &mut String) {
        match self {
            Self::Byte(_) => out.push('y'),
            Self::Bool(_) => out.push('b'),
            Self::I16(_) => out.push('n'),
            Self::U16(_) => out.push('q'),
            Self::I32(_) => out.push('i'),
            Self::U32(_) => out.push('u'),
            Self::I64(_) => out.push('x'),
            Self::U64(_) => out.push('t'),
            Self::F64(_) => out.push('d'),
            Self::Str(_) => out.push('s'),
            Self::Path(_) => out.push('o'),
            Self::Sig(_) => out.push('g'),
            Self::Variant(_) => out.push('v'),
            Self::Array(elem, _) => {
                out.push('a');
                out.push_str(elem);
            }
            Self::Struct(fields) => {
                out.push('(');
                for f in fields {
                    f.write_signature(out);
                }
                out.push(')');
            }
            Self::DictEntry(k, v) => {
                out.push('{');
                k.write_signature(out);
                v.write_signature(out);
                out.push('}');
            }
        }
    }
}

/// Alignment of the type starting with `code`.
fn alignment(code: u8) -> usize {
    match code {
        b'n' | b'q' => 2,
        b'b' | b'i' | b'u' | b's' | b'o' | b'a' | b'h' => 4,
        b'x' | b't' | b'd' | b'(' | b'{' => 8,
        _ => 1,
    }
}

/// Length of the first complete type in `sig`.
fn type_len(sig: &[u8], depth: usize) -> io::Result<usize> {
    if depth > MAX_DEPTH {
        return Err(invalid("signature nested too deeply"));
    }
    let close = match sig.first() {
        Some(b'a') => return Ok(1 + type_len(&sig[1..], depth + 1)?),
        Some(b'(') => b')',
        Some(b'{') => b'}',
        Some(b'y' | b'b' | b'n' | b'q' | b'i' | b'u' | b'x' | b't' | b'd' | b's' | b'o')
        | Some(b'g' | b'v') => return Ok(1),
        _ => return Err(invalid("bad signature")),
    };
    let mut i = 1;
    while sig.get(i) != Some(&close) {
        i += type_len(&sig[i..], depth + 1)?;
    }
    Ok(i + 1)
}

/// Little-endian marshaller; alignment is relative to the buffer start.
#[derive(Default)]
pub struct Encoder {
    pub buf: Vec<u8>,
}

impl Encoder {
    fn pad(&mut self, align: usize) {
        let len = self.buf.len().next_multiple_of(align);
        self.buf.resize(len, 0);
    }

    fn u32(&mut self, n: u32) {
        self.pad(4);
        self.buf.extend_from_slice(&n.to_le_bytes());
    }

    fn string(&mut self, s: &str) {
        // NUL is not allowed inside D-Bus strings (tags may carry one).
        let len = s.bytes().filter(|&b| b != 0).count();
        self.u32(len as u32);
        self.buf.extend(s.bytes().filter(|&b| b != 0));
        self.buf.push(0);
    }

    fn signature(&mut self, s: &str) {
        self.buf.push(s.len() as u8);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    pub fn put(&mut self, v: &Value) {
        match v {
            Value::Byte(b) => self.buf.push(*b),
            Value::Bool(b) => self.u32(u32::from(*b)),
            Value::I16(n) => {
                self.pad(2);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::U16(n) => {
                self.pad(2);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::I32(n) => {
                self.pad(4);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::U32(n) => self.u32(*n),
            Value::I64(n) => {
                self.pad(8);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::U64(n) => {
                self.pad(8);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::F64(n) => {
                self.pad(8);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::Str(s) | Value::Path(s) => self.string(s),
            Value::Sig(s) => self.signature(s),
            Value::Variant(inner) => {
                let mut sig = String::new();
                inner.write_signature(&mut sig);
                self.signature(&sig);
                self.put(inner);
            }
            Value::Array(elem, items) => {
                self.u32(0);
                let len_at = self.buf.len() - 4;
                self.pad(alignment(elem.as_bytes().first().copied().unwrap_or(b'y')));
                let start = self.buf.len();
                for item in items {
                    self.put(item);
                }
                let len = (self.buf.len() - start) as u32;
                self.buf[len_at..len_at + 4].copy_from_slice(&len.to_le_bytes());
            }
            Value::Struct(fields) => {
                self.pad(8);
                for f in fields {
                    self.put(f);
                }
            }
            Value::DictEntry(k, v) => {
                self.pad(8);
                self.put(k);
                self.put(v);
            }
        }
    }
}

/// Unmarshaller for either byte order.
struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
    big: bool,
}

impl<'a> Decoder<'a> {
    fn align(&mut self, align: usize) -> io::Result<()> {
        let pos = self.pos.next_multiple_of(align);
        if pos > self.buf.len() {
            return Err(invalid("truncated message"));
        }
        self.pos = pos;
        Ok(())
    }

    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or_else(|| invalid("truncated message"))?;
        let bytes = &self.buf[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn fixed<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        self.align(N)?;
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        if self.big {
            out.reverse();
        }
        Ok(out)
    }

    fn u32(&mut self) -> io::Result<u32> {
        self.fixed::<4>().map(u32::from_le_bytes)
    }

    fn text(&mut self, len: usize) -> io::Result<String> {
        let bytes = self.take(len)?;
        if self.take(1)? != [0] {
            return Err(invalid("unterminated string"));
        }
        String::from_utf8(bytes.to_vec()).map_err(|_| invalid("string is not UTF-8"))
    }

    fn signature(&mut self) -> io::Result<String> {
        let len = usize::from(self.take(1)?[0]);
        self.text(len)
    }

    /// Decode one value of the complete type `sig`.
    fn get(&mut self, sig: &[u8], depth: usize) -> io::Result<Value> {
        if depth > MAX_DEPTH {
            return Err(invalid("value nested too deeply"));
        }
        let Some(&code) = sig.first() else {
            return Err(invalid("empty signature"));
        };
        Ok(match code {
            b'y' => Value::Byte(self.take(1)?[0]),
            b'b' => match self.u32()? {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                _ => return Err(invalid("bad boolean")),
            },
            b'n' => Value::I16(i16::from_le_bytes(self.fixed()?)),
            b'q' => Value::U16(u16::from_le_bytes(self.fixed()?)),
            b'i' => Value::I32(i32::from_le_bytes(self.fixed()?)),
            b'u' => Value::U32(self.u32()?),
            b'x' => Value::I64(i64::from_le_bytes(self.fixed()?)),
            b't' => Value::U64(u64::from_le_bytes(self.fixed()?)),
            b'd' => Value::F64(f64::from_le_bytes(self.fixed()?)),
            b's' | b'o' => {
                let len = self.u32()? as usize;
                let s = self.text(len)?;
                if code == b's' {
                    Value::Str(s)
                } else {
                    Value::Path(s)
                }
            }
            b'g' => Value::Sig(self.signature()?),
            b'v' => {
                let sig = self.signature()?;
                let bytes = sig.as_bytes();
                if bytes.is_empty() || type_len(bytes, 0)? != bytes.len() {
                    return Err(invalid("variant must hold one type"));
                }
                Value::Variant(Box::new(self.get(bytes, depth + 1)?))
            }
            b'a' => {
                let elem = &sig[1..1 + type_len(&sig[1..], depth)?];
                let len = self.u32()? as usize;
                if len > 64 << 20 {
                    return Err(invalid("array too long"));
                }
                self.align(alignment(elem[0]))?;
                let end = self
                    .pos
                    .checked_add(len)
                    .filter(|&e| e <= self.buf.len())
                    .ok_or_else(|| invalid("truncated array"))?;
                let mut items = Vec::new();
                while self.pos < end {
                    items.push(self.get(elem, depth + 1)?);
                }
                if self.pos != end {
                    return Err(invalid("array length mismatch"));
                }
                let elem = std::str::from_utf8(elem).map_err(|_| invalid("bad signature"))?;
                Value::Array(elem.to_string(), items)
            }
            b'(' | b'{' => {
                self.align(8)?;
                let inner = &sig[1..type_len(sig, depth)? - 1];
                let mut fields = Vec::new();
                let mut i = 0;
                while i < inner.len() {
                    let n = type_len(&inner[i..], depth + 1)?;
                    fields.push(self.get(&inner[i..i + n], depth + 1)?);
                    i += n;
                }
                if code == b'(' {
                    Value::Struct(fields)
                } else {
                    let mut it = fields.into_iter();
                    match (it.next(), it.next(), it.next()) {
                        (Some(k), Some(v), None) => Value::DictEntry(Box::new(k), Box::new(v)),
                        _ => return Err(invalid("dict entry needs a key and a value")),
                    }
                }
            }
            _ => return Err(invalid("unsupported type")),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Message {
    pub kind: u8,
    pub flags: u8,
    pub serial: u32,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub error_name: Option<String>,
    pub reply_serial: Option<u32>,
    pub destination: Option<String>,
    pub sender: Option<String>,
    pub body: Vec<Value>,
}

impl Message {
    pub fn method_call(
        dest: &str,
        path: &str,
        iface: &str,
        member: &str,
        body: Vec<Value>,
    ) -> Self {
        Self {
            kind: METHOD_CALL,
            destination: Some(dest.into()),
            path: Some(path.into()),
            interface: Some(iface.into()),
            member: Some(member.into()),
            body,
            ..Self::default()
        }
    }

    pub fn signal(path: &str, iface: &str, member: &str, body: Vec<Value>) -> Self {
        Self {
            kind: SIGNAL,
            path: Some(path.into()),
            interface: Some(iface.into()),
            member: Some(member.into()),
            body,
            ..Self::default()
        }
    }

    pub fn method_return(call: &Message, body: Vec<Value>) -> Self {
        Self {
            kind: METHOD_RETURN,
            reply_serial: Some(call.serial),
            destination: call.sender.clone(),
            body,
            ..Self::default()
        }
    }

    pub fn error(call: &Message, name: &str, text: &str) -> Self {
        Self {
            kind: ERROR,
            error_name: Some(name.into()),
            reply_serial: Some(call.serial),
            destination: call.sender.clone(),
            body: vec![Value::Str(text.into())],
            ..Self::default()
        }
    }

    pub fn signature(&self) -> String {
        let mut sig = String::new();
        for v in &self.body {
            v.write_signature(&mut sig);
        }
        sig
    }

    pub fn encode(&self, serial: u32) -> Vec<u8> {
        let mut body = Encoder::default();
        for v in &self.body {
            body.put(v);
        }
        let mut fields = Vec::with_capacity(7);
        let mut field = |code: u8, v: Value| {
            fields.push(Value::Struct(vec![Value::Byte(code), Value::variant(v)]));
        };
        if let Some(p) = &self.path {
            field(1, Value::Path(p.clone()));
        }
        if let Some(i) = &self.interface {
            field(2, Value::Str(i.clone()));
        }
        if let Some(m) = &self.member {
            field(3, Value::Str(m.clone()));
        }
        if let Some(e) = &self.error_name {
            field(4, Value::Str(e.clone()));
        }
        if let Some(r) = self.reply_serial {
            field(5, Value::U32(r));
        }
        if let Some(d) = &self.destination {
            field(6, Value::Str(d.clone()));
        }
        let sig = self.signature();
        if !sig.is_empty() {
            field(8, Value::Sig(sig));
        }
        let mut out = Encoder {
            buf: vec![b'l', self.kind, self.flags, 1],
        };
        out.u32(body.buf.len() as u32);
        out.u32(serial);
        out.put(&Value::Array("(yv)".into(), fields));
        out.pad(8);
        out.buf.extend_from_slice(&body.buf);
        out.buf
    }

    pub fn decode(buf: &[u8]) -> io::Result<Self> {
        if buf.len() < 16 {
            return Err(invalid("short message"));
        }
        let big = match buf[0] {
            b'l' => false,
            b'B' => true,
            _ => return Err(invalid("bad byte order")),
        };
        if buf[3] != 1 {
            return Err(invalid("unsupported protocol version"));
        }
        let mut d = Decoder { buf, pos: 4, big };
        let body_len = d.u32()? as usize;
        let mut msg = Self {
            kind: buf[1],
            flags: buf[2],
            serial: d.u32()?,
            ..Self::default()
        };
        let mut signature = String::new();
        let Value::Array(_, fields) = d.get(b"a(yv)", 0)? else {
            return Err(invalid("bad header"));
        };
        for f in fields {
            let Value::Struct(f) = f else { continue };
            let (Some(Value::Byte(code)), Some(Value::Variant(v))) = (f.first(), f.get(1)) else {
                continue;
            };
            let text = || v.as_str().map(str::to_string);
            match code {
                1 => msg.path = text(),
                2 => msg.interface = text(),
                3 => msg.member = text(),
                4 => msg.error_name = text(),
                5 => msg.reply_serial = v.as_u32(),
                6 => msg.destination = text(),
                7 => msg.sender = text(),
                8 => signature = text().unwrap_or_default(),
                _ => {}
            }
        }
        d.align(8)?;
        if d.pos.checked_add(body_len) != Some(buf.len()) {
            return Err(invalid("body length mismatch"));
        }
        // The body starts 8-aligned, so offsets within it align the same.
        let mut body = Decoder {
            buf: &buf[d.pos..],
            pos: 0,
            big,
        };
        let sig = signature.as_bytes();
        let mut i = 0;
        while i < sig.len() {
            let n = type_len(&sig[i..], 0)?;
            msg.body.push(body.get(&sig[i..i + n], 0)?);
            i += n;
        }
        Ok(msg)
    }
}

/// Read one raw message (framing only).
pub fn read_message(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut head = [0u8; 16];
    r.read_exact(&mut head)?;
    let word = |at: usize| {
        let b = [head[at], head[at + 1], head[at + 2], head[at + 3]];
        if head[0] == b'B' {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        }
    };
    let body_len = word(4) as usize;
    let fields_len = word(12) as usize;
    let total = (16 + fields_len)
        .next_multiple_of(8)
        .checked_add(body_len)
        .filter(|&t| t <= MAX_MESSAGE)
        .ok_or_else(|| invalid("message too large"))?;
    let mut buf = Vec::with_capacity(total);
    buf.extend_from_slice(&head);
    buf.resize(total, 0);
    r.read_exact(&mut buf[16..])?;
    Ok(buf)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Address {
    Path(PathBuf),
    Abstract(Vec<u8>),
}

fn unescape(v: &str) -> Option<Vec<u8>> {
    let bytes = v.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// The unix-socket entries of a D-Bus address list (`;`-separated).
pub fn parse_addresses(list: &str) -> Vec<Address> {
    list.split(';')
        .filter_map(|entry| {
            let params = entry.trim().strip_prefix("unix:")?;
            params.split(',').find_map(|kv| match kv.split_once('=')? {
                ("path", v) => Some(Address::Path(PathBuf::from(OsString::from_vec(unescape(
                    v,
                )?)))),
                ("abstract", v) => Some(Address::Abstract(unescape(v)?)),
                _ => None,
            })
        })
        .collect()
}

/// `$DBUS_SESSION_BUS_ADDRESS`, else the systemd default `$XDG_RUNTIME_DIR/bus`.
pub fn session_addresses() -> Vec<Address> {
    match std::env::var("DBUS_SESSION_BUS_ADDRESS") {
        Ok(list) if !list.trim().is_empty() => parse_addresses(&list),
        _ => std::env::var_os("XDG_RUNTIME_DIR")
            .map(|dir| PathBuf::from(dir).join("bus"))
            .filter(|p| p.exists())
            .map(Address::Path)
            .into_iter()
            .collect(),
    }
}

fn connect(addr: &Address) -> io::Result<UnixStream> {
    match addr {
        Address::Path(p) => UnixStream::connect(p),
        Address::Abstract(name) => {
            use std::os::linux::net::SocketAddrExt;
            let addr = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
            UnixStream::connect_addr(&addr)
        }
    }
}

/// Read one `\r\n`-terminated auth line (byte-wise: the binary protocol
/// follows on the same socket).
fn read_line(stream: &mut UnixStream) -> io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while !line.ends_with(b"\r\n") {
        stream.read_exact(&mut byte)?;
        line.push(byte[0]);
        if line.len() > 4096 {
            return Err(invalid("auth line too long"));
        }
    }
    line.truncate(line.len() - 2);
    String::from_utf8(line).map_err(|_| invalid("auth line is not UTF-8"))
}

/// `AUTH EXTERNAL` with the uid as hex-encoded ASCII.
pub fn auth_line(uid: u32) -> String {
    let hex: String = uid
        .to_string()
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("AUTH EXTERNAL {hex}\r\n")
}

/// A session-bus connection during setup (blocking calls with a timeout).
pub struct Bus {
    stream: UnixStream,
    serial: u32,
}

impl Bus {
    /// Connect and say Hello. `NotFound` = there is no session bus.
    pub fn connect_session(timeout: Duration) -> io::Result<Self> {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "no D-Bus session bus address");
        for addr in session_addresses() {
            match connect(&addr) {
                Ok(stream) => return Self::handshake(stream, timeout),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn handshake(mut stream: UnixStream, timeout: Duration) -> io::Result<Self> {
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        stream.write_all(b"\0")?;
        stream.write_all(auth_line(uid).as_bytes())?;
        let reply = read_line(&mut stream)?;
        if !reply.starts_with("OK ") {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("D-Bus authentication rejected: {reply}"),
            ));
        }
        stream.write_all(b"BEGIN\r\n")?;
        let mut bus = Self { stream, serial: 0 };
        bus.call(Message::method_call(
            BUS_NAME,
            BUS_PATH,
            BUS_NAME,
            "Hello",
            Vec::new(),
        ))?;
        Ok(bus)
    }

    fn send(&mut self, msg: &Message) -> io::Result<u32> {
        self.serial += 1;
        self.stream.write_all(&msg.encode(self.serial))?;
        Ok(self.serial)
    }

    /// Send `msg` and wait for its reply, skipping anything else.
    fn call(&mut self, msg: Message) -> io::Result<Message> {
        let serial = self.send(&msg)?;
        loop {
            let reply = Message::decode(&read_message(&mut self.stream)?)?;
            if reply.reply_serial != Some(serial) {
                continue;
            }
            if reply.kind == ERROR {
                let name = reply.error_name.as_deref().unwrap_or("error");
                let text = reply.body.first().and_then(Value::as_str).unwrap_or("");
                return Err(io::Error::other(format!("{name}: {text}")));
            }
            return Ok(reply);
        }
    }

    /// Request a well-known name without queueing; true when we own it.
    pub fn request_name(&mut self, name: &str) -> io::Result<bool> {
        const DO_NOT_QUEUE: u32 = 4;
        let reply = self.call(Message::method_call(
            BUS_NAME,
            BUS_PATH,
            BUS_NAME,
            "RequestName",
            vec![Value::Str(name.into()), Value::U32(DO_NOT_QUEUE)],
        ))?;
        // 1 = primary owner, 4 = already the owner.
        Ok(matches!(
            reply.body.first().and_then(Value::as_u32),
            Some(1 | 4)
        ))
    }

    /// The socket (blocking, no timeouts) and the last serial used.
    pub fn into_parts(self) -> io::Result<(UnixStream, u32)> {
        self.stream.set_read_timeout(None)?;
        self.stream.set_write_timeout(None)?;
        Ok((self.stream, self.serial))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: &Message) -> Message {
        let bytes = msg.encode(7);
        let raw = read_message(&mut bytes.as_slice()).expect("framing");
        assert_eq!(raw, bytes);
        Message::decode(&raw).expect("decode")
    }

    #[test]
    fn method_call_roundtrips() {
        let mut call = Message::method_call(
            "org.example",
            "/org/example",
            "org.example.Iface",
            "Do",
            vec![
                Value::Str("hi".into()),
                Value::I64(-5),
                Value::Bool(true),
                Value::F64(0.25),
                Value::Path("/a/b".into()),
                Value::Byte(9),
            ],
        );
        let back = roundtrip(&call);
        assert_eq!(back.serial, 7);
        call.serial = 7;
        assert_eq!(back, call);
        assert_eq!(back.signature(), "sxbdoy");
    }

    #[test]
    fn nested_containers_roundtrip() {
        let body = vec![
            Value::Str("org.mpris.MediaPlayer2.Player".into()),
            Value::dict(vec![
                ("Volume", Value::F64(0.5)),
                ("Shuffle", Value::Bool(false)),
                (
                    "Metadata",
                    Value::dict(vec![
                        ("xesam:artist", Value::strings(vec!["A".into(), "B".into()])),
                        ("mpris:length", Value::I64(1_000_000)),
                    ]),
                ),
            ]),
            Value::strings(Vec::new()),
        ];
        let msg = Message::signal("/p", "i.f", "Changed", body.clone());
        let back = roundtrip(&msg);
        assert_eq!(back.body, body);
        assert_eq!(back.signature(), "sa{sv}as");
    }

    #[test]
    fn known_wire_bytes() {
        // Hello as libdbus sends it (serial 1), little-endian.
        let hello = Message::method_call(BUS_NAME, BUS_PATH, BUS_NAME, "Hello", Vec::new());
        let bytes = hello.encode(1);
        assert_eq!(&bytes[..12], &[b'l', 1, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(bytes.len() % 8, 0);
        let fields_len = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        assert_eq!(bytes.len(), (16 + fields_len as usize).next_multiple_of(8));
        // First field: PATH (code 1), variant signature "o".
        assert_eq!(&bytes[16..20], &[1, 1, b'o', 0]);
    }

    #[test]
    fn decodes_big_endian() {
        // METHOD_RETURN, serial 2, reply to 1, body "u" = 1 (big-endian).
        let fields = [
            5, 1, b'u', 0, 0, 0, 0, 1, // REPLY_SERIAL = 1
            8, 1, b'g', 0, 1, b'u', 0, // SIGNATURE = "u"
        ];
        let mut msg = vec![b'B', METHOD_RETURN, 0, 1, 0, 0, 0, 4, 0, 0, 0, 2];
        msg.extend_from_slice(&(fields.len() as u32).to_be_bytes());
        msg.extend_from_slice(&fields);
        msg.push(0); // header padding to 8
        msg.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(read_message(&mut msg.as_slice()).expect("framing"), msg);
        let m = Message::decode(&msg).expect("decode");
        assert_eq!(m.kind, METHOD_RETURN);
        assert_eq!(m.reply_serial, Some(1));
        assert_eq!(m.body, vec![Value::U32(1)]);
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        let good = Message::signal("/p", "i.f", "S", vec![Value::Str("x".into())]).encode(1);
        for cut in 0..good.len() {
            let _ = Message::decode(&good[..cut]);
        }
        let mut bad = good;
        bad[0] = b'x';
        assert!(Message::decode(&bad).is_err());
        assert!(type_len(b"a", 0).is_err());
        assert!(type_len(b"(ii", 0).is_err());
        assert_eq!(type_len(b"a{sv}x", 0).expect("len"), 5);
        assert_eq!(type_len(b"(yv)", 0).expect("len"), 4);
    }

    #[test]
    fn strings_drop_nul_bytes() {
        let msg = Message::signal("/p", "i.f", "S", vec![Value::Str("a\0b".into())]);
        let back = roundtrip(&msg);
        assert_eq!(back.body, vec![Value::Str("ab".into())]);
    }

    #[test]
    fn address_parsing() {
        assert_eq!(
            parse_addresses("unix:path=/run/user/1000/bus"),
            vec![Address::Path("/run/user/1000/bus".into())]
        );
        assert_eq!(
            parse_addresses("unix:abstract=/tmp/dbus-XYZ,guid=abc;unix:path=/tmp/my%20bus"),
            vec![
                Address::Abstract(b"/tmp/dbus-XYZ".to_vec()),
                Address::Path("/tmp/my bus".into())
            ]
        );
        assert!(parse_addresses("tcp:host=localhost,port=1").is_empty());
        assert!(parse_addresses("unix:path=/x%zz").is_empty());
    }

    #[test]
    fn auth_line_hex_encodes_uid() {
        assert_eq!(auth_line(1000), "AUTH EXTERNAL 31303030\r\n");
        assert_eq!(auth_line(0), "AUTH EXTERNAL 30\r\n");
    }
}
