use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Byte-oriented transport used by the ELM327 driver.
pub trait Transport: Send {
    fn write_bytes(&mut self, data: &[u8]) -> Result<()>;
    /// Returns Ok(None) on timeout with no data.
    fn read_byte(&mut self, timeout: Duration) -> Result<Option<u8>>;
    fn flush(&mut self) -> Result<()>;
    fn set_timeout(&mut self, timeout: Duration) -> Result<()>;
    fn description(&self) -> String;
}

pub struct SerialTransport {
    port: Box<dyn serialport::SerialPort>,
    timeout: Duration,
    path: String,
}

impl SerialTransport {
    pub fn open(path: &str, baud: u32, timeout: Duration) -> Result<Self> {
        let port = serialport::new(path, baud)
            .timeout(timeout)
            .open()
            .map_err(|e| Error::Transport(format!("cannot open {path} @ {baud}: {e}")))?;
        Ok(Self {
            port,
            timeout,
            path: path.to_string(),
        })
    }
}

impl Transport for SerialTransport {
    fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        self.port.write_all(data)?;
        std::io::Write::flush(&mut self.port)?;
        Ok(())
    }

    fn read_byte(&mut self, timeout: Duration) -> Result<Option<u8>> {
        self.port
            .set_timeout(timeout)
            .map_err(|e| Error::Transport(e.to_string()))?;
        let mut b = [0u8; 1];
        match self.port.read(&mut b) {
            Ok(1) => Ok(Some(b[0])),
            Ok(_) => Ok(None),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.port
            .clear(serialport::ClearBuffer::Input)
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(())
    }

    fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        self.timeout = timeout;
        self.port
            .set_timeout(timeout)
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(())
    }

    fn description(&self) -> String {
        format!(
            "serial {} @ {}",
            self.path,
            self.port.baud_rate().unwrap_or(0)
        )
    }
}

pub struct TcpTransport {
    stream: TcpStream,
    addr: String,
}

impl TcpTransport {
    pub fn open(addr: &str, timeout: Duration) -> Result<Self> {
        let sa = addr
            .to_socket_addrs()
            .map_err(|e| Error::Transport(format!("bad address {addr}: {e}")))?
            .next()
            .ok_or_else(|| Error::Transport(format!("cannot resolve {addr}")))?;
        let stream = TcpStream::connect_timeout(&sa, timeout)
            .map_err(|e| Error::Transport(format!("cannot connect {addr}: {e}")))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.set_nodelay(true).ok();
        Ok(Self {
            stream,
            addr: addr.to_string(),
        })
    }
}

impl Transport for TcpTransport {
    fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        self.stream.write_all(data)?;
        self.stream.flush()?;
        Ok(())
    }

    fn read_byte(&mut self, timeout: Duration) -> Result<Option<u8>> {
        self.stream.set_read_timeout(Some(timeout))?;
        let mut b = [0u8; 1];
        match self.stream.read(&mut b) {
            Ok(1) => Ok(Some(b[0])),
            Ok(_) => Ok(None),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn set_timeout(&mut self, _timeout: Duration) -> Result<()> {
        Ok(())
    }

    fn description(&self) -> String {
        format!("tcp {}", self.addr)
    }
}

/// In-memory transport used by the simulator and tests.
pub struct MemoryTransport {
    output: Vec<u8>,
    input: Vec<u8>,
    read_pos: usize,
    description: String,
    processor: Option<Box<dyn FnMut(&[u8]) -> Vec<u8> + Send>>,
}

impl MemoryTransport {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            output: Vec::new(),
            input: Vec::new(),
            read_pos: 0,
            description: description.into(),
            processor: None,
        }
    }

    pub fn with_processor(
        description: impl Into<String>,
        processor: Box<dyn FnMut(&[u8]) -> Vec<u8> + Send>,
    ) -> Self {
        Self {
            output: Vec::new(),
            input: Vec::new(),
            read_pos: 0,
            description: description.into(),
            processor: Some(processor),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.input.extend_from_slice(bytes);
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }
}

impl Transport for MemoryTransport {
    fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        self.output.extend_from_slice(data);
        if let Some(p) = &mut self.processor {
            let response = p(data);
            self.input.extend_from_slice(&response);
        }
        Ok(())
    }

    fn read_byte(&mut self, _timeout: Duration) -> Result<Option<u8>> {
        if self.read_pos < self.input.len() {
            let b = self.input[self.read_pos];
            self.read_pos += 1;
            Ok(Some(b))
        } else {
            Ok(None)
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.input.clear();
        self.read_pos = 0;
        Ok(())
    }

    fn set_timeout(&mut self, _timeout: Duration) -> Result<()> {
        Ok(())
    }

    fn description(&self) -> String {
        self.description.clone()
    }
}

/// Read until the ELM prompt '>' or timeout. Returns raw bytes (including prompt).
pub fn read_prompt(io: &mut dyn Transport, timeout: Duration) -> Result<Vec<u8>> {
    let start = Instant::now();
    let mut out = Vec::new();
    loop {
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            break;
        }
        let slice = (timeout - elapsed).min(Duration::from_millis(250));
        match io.read_byte(slice) {
            Ok(Some(b)) => {
                out.push(b);
                if b == b'>' {
                    break;
                }
            }
            Ok(None) => continue,
            Err(e) => return Err(e),
        }
    }
    if out.is_empty() {
        Err(Error::Timeout("no response from adapter".into()))
    } else {
        Ok(out)
    }
}

pub fn try_ports() -> Vec<String> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default()
}
