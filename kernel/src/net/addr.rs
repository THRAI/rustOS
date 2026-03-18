//! Socket address types and conversions.

use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address};

use crate::hal_common::{Errno, KernelResult};

/// IPv4 socket address (matches C `struct sockaddr_in`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockAddrIn4 {
    pub sin_family: u16,
    pub sin_port: u16,     // network byte order (big-endian)
    pub sin_addr: [u8; 4],
    pub sin_zero: [u8; 8],
}

impl SockAddrIn4 {
    pub const SIZE: usize = 16;

    /// Read a sockaddr_in from user memory.
    pub fn from_user(addr_ptr: usize, addr_len: usize) -> KernelResult<Self> {
        if addr_len < 8 {
            return Err(Errno::Einval);
        }
        let read_len = addr_len.min(Self::SIZE);
        let mut buf = [0u8; 16];
        let rc = unsafe {
            crate::hal::copy_user_chunk(
                buf.as_mut_ptr() as *mut u8,
                addr_ptr as *const u8,
                read_len,
            )
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
        Ok(Self {
            sin_family: u16::from_ne_bytes([buf[0], buf[1]]),
            sin_port: u16::from_be_bytes([buf[2], buf[3]]),
            sin_addr: [buf[4], buf[5], buf[6], buf[7]],
            sin_zero: [0; 8],
        })
    }

    /// Write this sockaddr_in to user memory.
    pub fn to_user(&self, addr_ptr: usize, addrlen_ptr: usize) -> KernelResult<()> {
        let mut buf = [0u8; 16];
        buf[0..2].copy_from_slice(&self.sin_family.to_ne_bytes());
        buf[2..4].copy_from_slice(&self.sin_port.to_be_bytes());
        buf[4..8].copy_from_slice(&self.sin_addr);

        let rc = unsafe {
            crate::hal::copy_user_chunk(addr_ptr as *mut u8, buf.as_ptr(), Self::SIZE)
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
        if addrlen_ptr != 0 {
            let len_bytes = (Self::SIZE as u32).to_ne_bytes();
            let rc = unsafe {
                crate::hal::copy_user_chunk(addrlen_ptr as *mut u8, len_bytes.as_ptr(), 4)
            };
            if rc != 0 {
                return Err(Errno::Efault);
            }
        }
        Ok(())
    }

    /// Convert to smoltcp IpEndpoint.
    pub fn to_endpoint(&self) -> IpEndpoint {
        let ip = Ipv4Address::new(
            self.sin_addr[0],
            self.sin_addr[1],
            self.sin_addr[2],
            self.sin_addr[3],
        );
        IpEndpoint::new(IpAddress::Ipv4(ip), self.sin_port)
    }

    /// Convert to smoltcp IpListenEndpoint (0.0.0.0 → addr=None).
    pub fn to_listen_endpoint(&self) -> IpListenEndpoint {
        let ip = Ipv4Address::new(
            self.sin_addr[0],
            self.sin_addr[1],
            self.sin_addr[2],
            self.sin_addr[3],
        );
        if ip == Ipv4Address::UNSPECIFIED {
            IpListenEndpoint {
                addr: None,
                port: self.sin_port,
            }
        } else {
            IpListenEndpoint {
                addr: Some(IpAddress::Ipv4(ip)),
                port: self.sin_port,
            }
        }
    }

    /// Construct from smoltcp IpEndpoint.
    pub fn from_endpoint(ep: &IpEndpoint) -> Self {
        let addr = match ep.addr {
            IpAddress::Ipv4(ip) => ip.0,
        };
        Self {
            sin_family: 2, // AF_INET
            sin_port: ep.port,
            sin_addr: addr,
            sin_zero: [0; 8],
        }
    }
}

/// Zero endpoint constant.
pub const ZERO_ENDPOINT: IpEndpoint = IpEndpoint {
    addr: IpAddress::Ipv4(Ipv4Address::UNSPECIFIED),
    port: 0,
};
